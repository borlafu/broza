//! Command implementations.
//!
//! `about`, `config`, `scan` and `explain` are complete. Everything that
//! *writes* to a disk parses fully and then reports [`not_implemented`]: the
//! detection and cleanup engines land in milestone M3.
//!
//! Every command returns an [`Outcome`]: the text to write to the sink, the
//! exit code, the warnings the envelope already carries (repeated on stderr
//! for a human), and the notes that exist only for a human.

pub mod about;
pub mod atomic;
pub mod config;
pub mod explain;
pub mod scan;

use broza::model::Warning;
use broza::{BrozaError, ExitCode};

/// Milestone that will implement the remaining disk-facing commands.
const PENDING_MILESTONE: &str = "milestone M3";

/// Everything one command produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// What goes to stdout, or to `--output`.
    pub rendered: String,
    /// Exit code of the run.
    pub code: ExitCode,
    /// Warnings of the envelope, repeated on stderr unless `--quiet`.
    pub warnings: Vec<Warning>,
    /// Human-only remarks. Never part of the JSON contract.
    pub notes: Vec<String>,
}

impl Outcome {
    /// A successful outcome carrying only its rendered text.
    pub const fn ok(rendered: String) -> Self {
        Self { rendered, code: ExitCode::Ok, warnings: Vec::new(), notes: Vec::new() }
    }

    /// The same outcome with `warnings` attached.
    #[must_use]
    pub fn with_warnings(self, warnings: Vec<Warning>) -> Self {
        Self { warnings, ..self }
    }

    /// The same outcome with `notes` attached.
    #[must_use]
    pub fn with_notes(self, notes: Vec<String>) -> Self {
        Self { notes, ..self }
    }

    /// The same outcome with an explicit exit code.
    #[must_use]
    pub fn with_code(self, code: ExitCode) -> Self {
        Self { code, ..self }
    }
}

/// Error returned by commands whose engine does not exist yet (exit code `1`).
pub fn not_implemented(command: &str) -> BrozaError {
    BrozaError::Other(format!("`broza {command}` is not implemented until {PENDING_MILESTONE}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn pending_commands_map_to_the_generic_exit_code() {
        let err = not_implemented("clean");
        assert_eq!(ExitCode::from(&err), ExitCode::GenericError);
        assert!(err.to_string().contains("not implemented"), "{err}");
        assert!(err.to_string().contains("clean"), "{err}");
    }

    #[test]
    fn an_outcome_succeeds_and_says_nothing_extra_by_default() {
        let outcome = Outcome::ok("text".to_owned());

        assert_eq!(outcome.code, ExitCode::Ok);
        assert!(outcome.warnings.is_empty());
        assert!(outcome.notes.is_empty());
    }

    #[test]
    fn the_builders_only_change_what_they_name() {
        let warning = Warning { code: "c".into(), message: "m".into(), path: None };
        let outcome = Outcome::ok("text".to_owned())
            .with_warnings(vec![warning.clone()])
            .with_notes(vec!["note".to_owned()])
            .with_code(ExitCode::PartialFailure);

        assert_eq!(outcome.rendered, "text");
        assert_eq!(outcome.warnings, vec![warning]);
        assert_eq!(outcome.notes, vec!["note".to_owned()]);
        assert_eq!(outcome.code, ExitCode::PartialFailure);
    }
}
