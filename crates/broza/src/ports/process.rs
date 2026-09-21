//! External command execution.

use std::time::Duration;

use crate::BrozaError;

/// Captured output of a finished command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    /// `true` when the process exited with status 0.
    pub success: bool,
    /// Exit code when the process exited normally.
    pub code: Option<i32>,
    /// Captured standard output.
    pub stdout: Vec<u8>,
    /// Captured standard error.
    pub stderr: Vec<u8>,
}

impl ProcessOutput {
    /// Standard output as lossy UTF-8.
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// Standard error as lossy UTF-8.
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// Runs an external command with a hard timeout.
///
/// Implementations must kill the child when `timeout` elapses and return an error.
pub trait ProcessRunner: Send + Sync {
    /// Run `program` with `args`, capturing output.
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> Result<ProcessOutput, BrozaError>;
}

/// A shared runner is a runner.
///
/// Adapters that own a runner are generic over it rather than boxed, so two of
/// them sharing one runner — the disk enumerator and the snapshot provider do —
/// need `Arc<R>` to satisfy the same bound as `R`.
impl<T: ProcessRunner + ?Sized> ProcessRunner for std::sync::Arc<T> {
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> Result<ProcessOutput, BrozaError> {
        (**self).run(program, args, timeout)
    }
}
