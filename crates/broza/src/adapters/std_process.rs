//! Real [`ProcessRunner`].
//!
//! Two properties matter here and both are load bearing.
//!
//! *No deadlock*: each pipe is drained by its own thread, so a command that fills
//! one of them cannot block Broza (`diskutil apfs list -plist` on a large container
//! easily exceeds a pipe buffer).
//!
//! *A hard deadline*: `timeout` bounds the whole call, not just the direct child.
//! The child leads its own process group, and nothing — waiting for it, or reading
//! its output — is ever waited on without a bound. A grandchild that inherits the
//! pipe and sleeps for an hour (`sh -c 'sleep 3600 & echo done'`) therefore costs
//! the timeout and no more: the group is killed and the call fails.

use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use crate::BrozaError;
use crate::adapters::process_error::{ProcessError, ProcessErrorKind};
use crate::ports::{ProcessOutput, ProcessRunner};

/// How often the runner checks whether the child has finished.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Deadline used when `timeout` is so large that adding it would overflow.
const FAR_FUTURE: Duration = Duration::from_secs(365 * 24 * 60 * 60);
/// How long a killed command is given to let its pipe readers finish.
const REAP_GRACE: Duration = Duration::from_millis(50);
/// Process group argument that makes the child the leader of a new group.
const OWN_PROCESS_GROUP: i32 = 0;

/// What one pipe thread eventually delivers.
type PipeResult = Receiver<std::io::Result<Vec<u8>>>;

/// A bounded budget shared by every wait in one `run`.
#[derive(Debug, Clone, Copy)]
struct Deadline {
    /// When the budget is spent.
    at: Instant,
    /// The budget itself, for the error message.
    budget: Duration,
}

impl Deadline {
    /// Start a budget of `budget` now, never panicking on an absurd duration.
    fn start(budget: Duration) -> Self {
        let now = Instant::now();
        let at = now.checked_add(budget).or_else(|| now.checked_add(FAR_FUTURE)).unwrap_or(now);
        Self { at, budget }
    }

    /// Time left, zero once the budget is spent.
    fn remaining(self) -> Duration {
        self.at.saturating_duration_since(Instant::now())
    }
}

/// The two pipe threads of a running child.
struct Pipes {
    /// Standard output.
    stdout: PipeResult,
    /// Standard error.
    stderr: PipeResult,
}

/// [`ProcessRunner`] that really spawns processes.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdProcessRunner;

impl ProcessRunner for StdProcessRunner {
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> Result<ProcessOutput, BrozaError> {
        let deadline = Deadline::start(timeout);
        let mut child = spawn(program, args)?;
        let pipes = match capture(&mut child, program) {
            Ok(pipes) => pipes,
            Err(error) => {
                terminate(&mut child, None);
                return Err(error);
            }
        };
        match collect(&mut child, &pipes, program, deadline) {
            Ok(output) => Ok(output),
            Err(error) => {
                terminate(&mut child, Some(pipes));
                Err(error)
            }
        }
    }
}

/// Start `program` as the leader of its own process group, with both pipes captured.
///
/// The group is what makes the deadline enforceable: killing the child alone would
/// leave anything it started holding the pipe open.
fn spawn(program: &str, args: &[&str]) -> Result<Child, BrozaError> {
    use std::os::unix::process::CommandExt;

    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(OWN_PROCESS_GROUP)
        .spawn()
        .map_err(|source| match source.kind() {
            std::io::ErrorKind::NotFound => ProcessError::new(ProcessErrorKind::NotFound, program).into(),
            std::io::ErrorKind::PermissionDenied => {
                ProcessError::new(ProcessErrorKind::NotExecutable, program).into()
            }
            _ => BrozaError::Io { context: format!("run `{program}`"), source },
        })
}

/// Start draining both pipes.
fn capture(child: &mut Child, program: &str) -> Result<Pipes, BrozaError> {
    let stdout = drain(child.stdout.take(), program)?;
    let stderr = drain(child.stderr.take(), program)?;
    Ok(Pipes { stdout, stderr })
}

/// Read one pipe to the end on its own thread, delivering the result over a channel.
///
/// A channel rather than a join handle: the reader can be abandoned without
/// blocking, which is what a timed-out command needs.
fn drain<R>(pipe: Option<R>, program: &str) -> Result<PipeResult, BrozaError>
where
    R: Read + Send + 'static,
{
    let mut pipe = pipe.ok_or_else(|| ProcessError::new(ProcessErrorKind::OutputUnavailable, program))?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = Vec::new();
        let result = pipe.read_to_end(&mut buffer).map(|_| buffer);
        let _ = sender.send(result);
    });
    Ok(receiver)
}

/// Wait for the child and its output, all within `deadline`.
fn collect(
    child: &mut Child,
    pipes: &Pipes,
    program: &str,
    deadline: Deadline,
) -> Result<ProcessOutput, BrozaError> {
    let status = wait_until(child, deadline)
        .ok_or_else(|| ProcessError::timed_out(program, deadline.budget))?
        .map_err(|source| BrozaError::Io { context: format!("wait for `{program}`"), source })?;
    Ok(ProcessOutput {
        success: status.success(),
        code: status.code(),
        stdout: receive(&pipes.stdout, deadline, program)?,
        stderr: receive(&pipes.stderr, deadline, program)?,
    })
}

/// Wait for the child until the deadline passes; `None` means the deadline won.
fn wait_until(child: &mut Child, deadline: Deadline) -> Option<std::io::Result<ExitStatus>> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(Ok(status)),
            Ok(None) => {}
            Err(error) => return Some(Err(error)),
        }
        let remaining = deadline.remaining();
        if remaining.is_zero() {
            return None;
        }
        thread::sleep(POLL_INTERVAL.min(remaining));
    }
}

/// Take what a pipe thread read, waiting no longer than the deadline allows.
fn receive(pipe: &PipeResult, deadline: Deadline, program: &str) -> Result<Vec<u8>, BrozaError> {
    match pipe.recv_timeout(deadline.remaining()) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(source)) => Err(BrozaError::Io { context: format!("read output of `{program}`"), source }),
        Err(RecvTimeoutError::Timeout) => Err(ProcessError::timed_out(program, deadline.budget).into()),
        Err(RecvTimeoutError::Disconnected) => {
            Err(ProcessError::new(ProcessErrorKind::OutputLost, program).into())
        }
    }
}

/// Kill the child's whole process group, reap it, and let the readers go.
///
/// The readers get a short grace period so they normally finish on their own; after
/// that they are abandoned, because waiting for them is exactly the unbounded wait
/// the deadline exists to prevent. They end by themselves once the pipes close.
fn terminate(child: &mut Child, pipes: Option<Pipes>) {
    kill_process_group(child);
    let _ = child.kill();
    let _ = child.wait();
    if let Some(pipes) = pipes {
        let grace = Deadline::start(REAP_GRACE);
        let _ = pipes.stdout.recv_timeout(grace.remaining());
        let _ = pipes.stderr.recv_timeout(grace.remaining());
    }
}

/// Send `SIGKILL` to the process group led by `child`.
#[allow(unsafe_code)]
fn kill_process_group(child: &Child) {
    let Ok(pid) = i32::try_from(child.id()) else { return };
    if pid <= 0 {
        return;
    }
    // SAFETY: `killpg` takes a process group id and a signal number and touches no
    // memory. The child was spawned with `process_group(0)`, so its group id equals
    // its pid, and the pid belongs to a child of this process that has not been
    // reaped yet, so it cannot have been recycled by another program.
    let _ = unsafe { libc::killpg(pid, libc::SIGKILL) };
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Deadline, FAR_FUTURE, StdProcessRunner};
    use crate::BrozaError;
    use crate::ports::ProcessRunner;

    /// Generous timeout for commands that are expected to finish at once.
    const PATIENT: Duration = Duration::from_secs(10);
    /// Timeout the deadline tests give a command that will never finish in time.
    const IMPATIENT: Duration = Duration::from_millis(200);
    /// Upper bound on how long an impatient call may take, including process setup.
    const SLACK: Duration = Duration::from_secs(1);
    /// Bytes the deadlock test pushes through the pipe.
    const LARGE_OUTPUT_BYTES: usize = 1024 * 1024;

    fn timed_out_within(args: &[&str], limit: Duration) -> String {
        let started = Instant::now();
        let err = StdProcessRunner.run("/bin/sh", args, IMPATIENT).err();
        let elapsed = started.elapsed();
        let Some(BrozaError::Other(message)) = err else { panic!("expected BrozaError::Other") };
        assert!(message.contains("timed out"), "{message}");
        assert!(elapsed < limit, "returned only after {elapsed:?}");
        message
    }

    #[test]
    fn a_successful_command_reports_its_output_and_exit_code() {
        let out = StdProcessRunner.run("/bin/echo", &["hello"], PATIENT).unwrap_or_else(|e| panic!("{e}"));

        assert!(out.success);
        assert_eq!(out.code, Some(0));
        assert_eq!(out.stdout_text(), "hello\n");
        assert_eq!(out.stderr_text(), "");
    }

    #[test]
    fn a_failing_command_is_returned_as_output_not_as_an_error() {
        let out = StdProcessRunner.run("/usr/bin/false", &[], PATIENT).unwrap_or_else(|e| panic!("{e}"));

        assert!(!out.success);
        assert_eq!(out.code, Some(1));
    }

    #[test]
    fn standard_error_is_captured_separately() {
        let args = ["-c", "echo out; echo err >&2"];
        let out = StdProcessRunner.run("/bin/sh", &args, PATIENT).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(out.stdout_text(), "out\n");
        assert_eq!(out.stderr_text(), "err\n");
    }

    #[test]
    fn a_command_that_outlives_its_timeout_is_killed_quickly() {
        let started = Instant::now();

        let err = StdProcessRunner.run("/bin/sleep", &["30"], IMPATIENT).err();

        let elapsed = started.elapsed();
        let Some(BrozaError::Other(message)) = err else { panic!("expected BrozaError::Other") };
        assert!(message.contains("timed out"), "{message}");
        assert!(message.contains("/bin/sleep"), "{message}");
        assert!(elapsed < SLACK, "killed only after {elapsed:?}");
    }

    #[test]
    fn a_grandchild_holding_the_pipe_cannot_outlive_the_timeout() {
        // The shell exits at once; `sleep` inherits its standard output and would
        // keep the pipe open for half a minute if the deadline were not enforced on
        // reading too, and if the kill did not reach the whole process group.
        timed_out_within(&["-c", "sleep 30 & echo started"], SLACK);
    }

    #[test]
    fn a_command_that_never_writes_and_never_exits_is_killed() {
        timed_out_within(&["-c", "exec sleep 30"], SLACK);
    }

    #[test]
    fn a_megabyte_of_output_does_not_deadlock_the_pipe() {
        let script = format!("yes | head -c {LARGE_OUTPUT_BYTES}");
        let args = ["-c", script.as_str()];

        let out = StdProcessRunner.run("/bin/sh", &args, PATIENT).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(out.stdout.len(), LARGE_OUTPUT_BYTES);
    }

    #[test]
    fn a_missing_program_is_reported_clearly() {
        let err = StdProcessRunner.run("/nonexistent/broza-ghost", &[], PATIENT).err();

        let Some(BrozaError::Other(message)) = err else { panic!("expected BrozaError::Other") };
        assert!(message.contains("not found"), "{message}");
        assert!(message.contains("/nonexistent/broza-ghost"), "{message}");
    }

    #[test]
    fn a_program_without_the_execute_bit_is_reported_clearly() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let path = dir.path().join("not-executable");
        std::fs::write(&path, b"#!/bin/sh\n").unwrap_or_else(|e| panic!("{e}"));

        let err = StdProcessRunner.run(&path.to_string_lossy(), &[], PATIENT).err();

        let Some(BrozaError::Other(message)) = err else { panic!("expected BrozaError::Other") };
        assert!(message.contains("not executable"), "{message}");
    }

    #[test]
    fn an_absurd_timeout_does_not_overflow_the_clock() {
        let deadline = Deadline::start(Duration::MAX);

        assert!(deadline.remaining() > FAR_FUTURE / 2);
        assert!(Deadline::start(Duration::ZERO).remaining().is_zero());
    }
}
