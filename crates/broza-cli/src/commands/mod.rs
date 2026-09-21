//! Command implementations.
//!
//! Every command of `docs/cli-spec.md` §3 is implemented; [`not_implemented`]
//! remains for the one execution path that is not (`clean --apply --purge`,
//! which waits for the irreversible path of the mover in M4).
//!
//! Every command returns an [`Outcome`]: the text to write to the sink, the
//! exit code, and the warnings the envelope already carries, which
//! [`crate::reporting`] repeats on stderr for a human.

pub mod about;
pub mod atomic;
pub mod clean;
pub mod clean_expiry;
pub mod clean_output;
#[cfg(test)]
mod clean_tests;
pub mod config;
pub mod detection;
pub mod explain;
pub mod mount;
pub mod quarantine;
#[cfg(test)]
mod quarantine_tests;
pub mod restore;
pub mod scan;
pub mod store;
pub mod suggest;
pub mod target;
#[cfg(test)]
pub(crate) mod test_world;

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
    /// What an applied cleanup moved or freed; `None` for every other run.
    /// This is condition 1 of the donation gate (`docs/cli-spec.md` §5).
    pub reclaimed: Option<Reclaimed>,
}

/// The two figures an applied cleanup reports, never summed for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reclaimed {
    /// Bytes moved into quarantine: pending until expiry or purge.
    pub quarantined_bytes: u64,
    /// Bytes actually freed: expired or purged sessions.
    pub freed_bytes: u64,
}

impl Reclaimed {
    /// Bytes made reclaimable in all, for the gate's "at least one byte".
    pub const fn total(self) -> u64 {
        self.quarantined_bytes.saturating_add(self.freed_bytes)
    }
}

impl Outcome {
    /// A successful outcome carrying only its rendered text.
    pub const fn ok(rendered: String) -> Self {
        Self { rendered, code: ExitCode::Ok, warnings: Vec::new(), reclaimed: None }
    }

    /// The same outcome, recorded as an applied cleanup that moved or freed `reclaimed`.
    #[must_use]
    pub fn with_reclaimed(self, reclaimed: Reclaimed) -> Self {
        Self { reclaimed: Some(reclaimed), ..self }
    }

    /// The same outcome with `warnings` attached.
    #[must_use]
    pub fn with_warnings(self, warnings: Vec<Warning>) -> Self {
        Self { warnings, ..self }
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
    }

    #[test]
    fn the_builders_only_change_what_they_name() {
        let warning = Warning { code: "c".into(), message: "m".into(), path: None };
        let outcome = Outcome::ok("text".to_owned())
            .with_warnings(vec![warning.clone()])
            .with_code(ExitCode::PartialFailure);

        assert_eq!(outcome.rendered, "text");
        assert_eq!(outcome.warnings, vec![warning]);
        assert_eq!(outcome.code, ExitCode::PartialFailure);
    }
}
