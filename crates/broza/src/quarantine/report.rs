//! A payload together with the diagnostics that belong beside it.
//!
//! The core never builds an [`Envelope`](crate::model::Envelope) — that is the
//! CLI's job — but the store is where a partial failure is *discovered*, so it
//! has to hand the `errors[]` and `warnings[]` entries up rather than print
//! them. A non-empty `errors` means exit `5` (`docs/cli-spec.md` §4.1).

use std::path::Path;

use crate::model::{Diagnostic, ErrorEntry, Warning};

/// A payload plus the envelope entries the operation produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reported<T> {
    /// What the command returns as `data`.
    pub data: T,
    /// Entries for `errors[]`; a non-empty list means the run was partial.
    pub errors: Vec<ErrorEntry>,
    /// Entries for `warnings[]`; these never change the exit code.
    pub warnings: Vec<Warning>,
}

impl<T> Reported<T> {
    /// A payload with no diagnostics at all.
    pub fn new(data: T) -> Self {
        Self { data, errors: Vec::new(), warnings: Vec::new() }
    }

    /// A payload with the diagnostics collected while producing it.
    pub fn with(data: T, errors: Vec<ErrorEntry>, warnings: Vec<Warning>) -> Self {
        Self { data, errors, warnings }
    }

    /// `true` when something in the run failed or was skipped.
    pub fn is_partial(&self) -> bool {
        !self.errors.is_empty()
    }
}

/// One `errors[]` or `warnings[]` entry about `path`.
pub fn diagnostic(code: &str, message: String, path: Option<&Path>) -> Diagnostic {
    Diagnostic { code: code.to_owned(), message, path: path.map(Path::to_path_buf) }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Reported, diagnostic};

    #[test]
    fn a_payload_without_diagnostics_is_not_partial() {
        let reported = Reported::new(7_u8);

        assert!(!reported.is_partial());
        assert_eq!(reported.data, 7);
    }

    #[test]
    fn an_error_entry_makes_the_run_partial_and_a_warning_does_not() {
        let entry = diagnostic("session_busy", "busy".to_owned(), Some(Path::new("/store/a")));

        assert!(Reported::with(0_u8, vec![entry.clone()], Vec::new()).is_partial());
        assert!(!Reported::with(0_u8, Vec::new(), vec![entry.clone()]).is_partial());
        assert_eq!(entry.path.as_deref(), Some(Path::new("/store/a")));
        assert_eq!(entry.code, "session_busy");
    }
}
