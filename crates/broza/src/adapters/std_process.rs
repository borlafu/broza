//! Real [`ProcessRunner`](crate::ports::ProcessRunner).
//!
//! Both pipes are drained by their own thread, so a command that fills one of them
//! cannot deadlock Broza (`diskutil apfs list -plist` on a large container easily
//! exceeds a pipe buffer). The timeout is a hard deadline: when it passes the child
//! is killed and the call fails, so an unresponsive `diskutil` can never hang a scan.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::BrozaError;
use crate::ports::{ProcessOutput, ProcessRunner};

/// How often the runner checks whether the child has finished.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// A thread draining one of the child's pipes.
type PipeReader = JoinHandle<std::io::Result<Vec<u8>>>;

/// [`ProcessRunner`] that really spawns processes.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdProcessRunner;

impl ProcessRunner for StdProcessRunner {
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> Result<ProcessOutput, BrozaError> {
        let mut child = spawn(program, args)?;
        let stdout = drain(child.stdout.take(), program, "stdout")?;
        let stderr = drain(child.stderr.take(), program, "stderr")?;
        let Some(status) = wait_until(&mut child, timeout) else {
            return Err(kill(&mut child, [stdout, stderr], program, timeout));
        };
        let status =
            status.map_err(|source| BrozaError::Io { context: format!("wait for `{program}`"), source })?;
        Ok(ProcessOutput {
            success: status.success(),
            code: status.code(),
            stdout: join(stdout, program)?,
            stderr: join(stderr, program)?,
        })
    }
}

/// Start `program` with both pipes captured and no standard input.
fn spawn(program: &str, args: &[&str]) -> Result<Child, BrozaError> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| match source.kind() {
            std::io::ErrorKind::NotFound => BrozaError::Other(format!("program not found: {program}")),
            std::io::ErrorKind::PermissionDenied => {
                BrozaError::Other(format!("program not executable: {program}"))
            }
            _ => BrozaError::Io { context: format!("run `{program}`"), source },
        })
}

/// Read one pipe to the end on its own thread.
fn drain<R>(pipe: Option<R>, program: &str, name: &str) -> Result<PipeReader, BrozaError>
where
    R: Read + Send + 'static,
{
    let mut pipe = pipe.ok_or_else(|| BrozaError::Other(format!("`{program}`: {name} was not captured")))?;
    Ok(thread::spawn(move || {
        let mut buffer = Vec::new();
        pipe.read_to_end(&mut buffer)?;
        Ok(buffer)
    }))
}

/// Kill a child that outlived its deadline and describe what happened.
///
/// The pipe threads are joined first: they end as soon as the kill closes the pipes,
/// and leaving them running would leak a thread per timed-out command.
fn kill(child: &mut Child, readers: [PipeReader; 2], program: &str, timeout: Duration) -> BrozaError {
    let _ = child.kill();
    let _ = child.wait();
    for reader in readers {
        let _ = reader.join();
    }
    BrozaError::Other(format!("`{program}` timed out after {} ms and was killed", timeout.as_millis()))
}

/// Wait for the child until `timeout` passes; `None` means the deadline won.
fn wait_until(child: &mut Child, timeout: Duration) -> Option<std::io::Result<std::process::ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(Ok(status)),
            Ok(None) => {}
            Err(error) => return Some(Err(error)),
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// Collect what a pipe thread read.
fn join(reader: PipeReader, program: &str) -> Result<Vec<u8>, BrozaError> {
    match reader.join() {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(source)) => Err(BrozaError::Io { context: format!("read output of `{program}`"), source }),
        Err(_) => Err(BrozaError::Other(format!("`{program}`: output reader panicked"))),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::BrozaError;
    use crate::adapters::StdProcessRunner;
    use crate::ports::ProcessRunner;

    /// Generous timeout for commands that are expected to finish at once.
    const PATIENT: Duration = Duration::from_secs(10);
    /// Bytes the deadlock test pushes through the pipe.
    const LARGE_OUTPUT_BYTES: usize = 1024 * 1024;

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

        let err = StdProcessRunner.run("/bin/sleep", &["5"], Duration::from_millis(100)).err();

        let elapsed = started.elapsed();
        let Some(BrozaError::Other(message)) = err else { panic!("expected BrozaError::Other") };
        assert!(message.contains("timed out"), "{message}");
        assert!(message.contains("/bin/sleep"), "{message}");
        assert!(elapsed < Duration::from_secs(2), "killed after {elapsed:?}");
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
}
