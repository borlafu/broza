//! Why running an external command failed.
//!
//! These are not I/O failures of a running process but failures to run one at all,
//! and they are the four cases Broza can explain to a user. They collapse into
//! [`BrozaError::Other`] (exit code `1`) for now; when the exit-code table of
//! `docs/cli-spec.md` §5 grows a code for "the tool Broza needs did not run", this
//! enum is what it maps from.

use std::fmt;
use std::time::Duration;

use crate::BrozaError;

/// The kind of failure to run a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProcessErrorKind {
    /// The program does not exist at that path.
    NotFound,
    /// The program exists but cannot be executed.
    NotExecutable,
    /// The command, or something it started, outlived its deadline and was killed.
    TimedOut {
        /// Budget the command was given.
        after: Duration,
    },
    /// The child's standard output or error could not be captured.
    OutputUnavailable,
    /// The thread reading one of the pipes ended without delivering anything.
    OutputLost,
}

/// A command Broza could not run, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessError {
    /// What went wrong.
    pub kind: ProcessErrorKind,
    /// Program Broza tried to run.
    pub program: String,
}

impl ProcessError {
    /// Describe a failure to run `program`.
    pub fn new(kind: ProcessErrorKind, program: &str) -> Self {
        Self { kind, program: program.to_owned() }
    }

    /// Describe a command killed after `after`.
    pub fn timed_out(program: &str, after: Duration) -> Self {
        Self::new(ProcessErrorKind::TimedOut { after }, program)
    }
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let program = &self.program;
        match self.kind {
            ProcessErrorKind::NotFound => write!(f, "program not found: {program}"),
            ProcessErrorKind::NotExecutable => write!(f, "program not executable: {program}"),
            ProcessErrorKind::TimedOut { after } => {
                write!(f, "`{program}` timed out after {} ms and was killed", after.as_millis())
            }
            ProcessErrorKind::OutputUnavailable => {
                write!(f, "`{program}`: its output could not be captured")
            }
            ProcessErrorKind::OutputLost => {
                write!(f, "`{program}`: the thread reading its output ended unexpectedly")
            }
        }
    }
}

impl From<ProcessError> for BrozaError {
    fn from(error: ProcessError) -> Self {
        Self::Other(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{ProcessError, ProcessErrorKind};
    use crate::BrozaError;

    #[test]
    fn every_kind_names_the_program_and_what_went_wrong() {
        let cases = [
            (ProcessErrorKind::NotFound, "program not found: diskutil"),
            (ProcessErrorKind::NotExecutable, "program not executable: diskutil"),
            (ProcessErrorKind::OutputUnavailable, "`diskutil`: its output could not be captured"),
            (ProcessErrorKind::OutputLost, "`diskutil`: the thread reading its output ended unexpectedly"),
        ];
        for (kind, expected) in cases {
            assert_eq!(ProcessError::new(kind, "diskutil").to_string(), expected);
        }
    }

    #[test]
    fn a_timeout_reports_the_budget_it_exceeded() {
        let error = ProcessError::timed_out("diskutil", Duration::from_millis(250));

        assert_eq!(error.to_string(), "`diskutil` timed out after 250 ms and was killed");
        assert_eq!(error.kind, ProcessErrorKind::TimedOut { after: Duration::from_millis(250) });
    }

    #[test]
    fn a_process_error_becomes_a_generic_broza_error_for_now() {
        let error = BrozaError::from(ProcessError::new(ProcessErrorKind::NotFound, "tmutil"));

        assert!(matches!(error, BrozaError::Other(message) if message.contains("tmutil")));
    }
}
