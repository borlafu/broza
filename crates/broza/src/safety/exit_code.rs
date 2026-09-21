//! Process exit codes (`docs/cli-spec.md` §2).

use crate::BrozaError;

/// Exit codes of the `broza` binary. Single code space for every command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ExitCode {
    /// Success, including "nothing to clean".
    Ok = 0,
    /// Unclassified error.
    GenericError = 1,
    /// Invalid arguments or flags.
    UsageError = 2,
    /// Missing Full Disk Access or permission on a path.
    PermissionDenied = 3,
    /// Volume, path, or quarantine item not found.
    TargetNotFound = 4,
    /// Operation finished but some items failed (see `errors[]`).
    PartialFailure = 5,
    /// User answered "no".
    AbortedByUser = 6,
    /// Confirmation required, no TTY, no `--yes`.
    ConfirmationRequired = 7,
    /// Unsupported macOS version or architecture.
    UnsupportedSystem = 8,
    /// Cache corrupted or unreadable; retry with `--no-cache`.
    CacheError = 9,
}

impl ExitCode {
    /// Numeric process exit code.
    pub const fn code(self) -> i32 {
        self as i32
    }

    /// Stable symbolic name as used in the specification.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::GenericError => "GENERIC_ERROR",
            Self::UsageError => "USAGE_ERROR",
            Self::PermissionDenied => "PERMISSION_DENIED",
            Self::TargetNotFound => "TARGET_NOT_FOUND",
            Self::PartialFailure => "PARTIAL_FAILURE",
            Self::AbortedByUser => "ABORTED_BY_USER",
            Self::ConfirmationRequired => "CONFIRMATION_REQUIRED",
            Self::UnsupportedSystem => "UNSUPPORTED_SYSTEM",
            Self::CacheError => "CACHE_ERROR",
        }
    }
}

impl From<&BrozaError> for ExitCode {
    fn from(err: &BrozaError) -> Self {
        match err {
            BrozaError::Usage(_) | BrozaError::Config(_) => Self::UsageError,
            BrozaError::PermissionDenied { .. } => Self::PermissionDenied,
            BrozaError::TargetNotFound(_) => Self::TargetNotFound,
            BrozaError::PartialFailure { .. } => Self::PartialFailure,
            BrozaError::AbortedByUser => Self::AbortedByUser,
            BrozaError::ConfirmationRequired => Self::ConfirmationRequired,
            BrozaError::UnsupportedSystem(_) => Self::UnsupportedSystem,
            BrozaError::Cache(_) => Self::CacheError,
            BrozaError::Io { .. } | BrozaError::Other(_) => Self::GenericError,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_specification() {
        assert_eq!(ExitCode::Ok.code(), 0);
        assert_eq!(ExitCode::ConfirmationRequired.code(), 7);
        assert_eq!(ExitCode::CacheError.code(), 9);
    }

    #[test]
    fn errors_map_to_expected_codes() {
        let cases: Vec<(BrozaError, ExitCode)> = vec![
            (BrozaError::Usage("x".into()), ExitCode::UsageError),
            (BrozaError::AbortedByUser, ExitCode::AbortedByUser),
            (BrozaError::ConfirmationRequired, ExitCode::ConfirmationRequired),
            (BrozaError::Cache("bad".into()), ExitCode::CacheError),
            (BrozaError::Other("x".into()), ExitCode::GenericError),
        ];
        for (err, expected) in cases {
            assert_eq!(ExitCode::from(&err), expected, "{err}");
        }
    }
}
