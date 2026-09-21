//! Command implementations.
//!
//! `about` and `config` are complete. Everything that reads disks or writes to
//! them parses fully and then reports [`not_implemented`]: the analysis and
//! cleanup engines land in milestones M2 and M3.

pub mod about;
pub mod atomic;
pub mod config;

use broza::BrozaError;

/// Milestone that will implement the disk-facing commands.
const PENDING_MILESTONE: &str = "milestone M2/M3";

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
        let err = not_implemented("scan");
        assert_eq!(broza::ExitCode::from(&err), broza::ExitCode::GenericError);
        assert!(err.to_string().contains("not implemented"), "{err}");
        assert!(err.to_string().contains("scan"), "{err}");
    }
}
