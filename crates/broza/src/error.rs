//! Single error type of the core. Every variant maps to exactly one [`crate::ExitCode`].

use std::path::PathBuf;

/// Errors produced by the Broza core.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BrozaError {
    /// Invalid arguments or flag combination.
    #[error("usage error: {0}")]
    Usage(String),
    /// Missing permission (for example, Full Disk Access) on a path.
    #[error("permission denied: {path}")]
    PermissionDenied {
        /// Path that could not be accessed.
        path: PathBuf,
    },
    /// Volume, path, or quarantine item does not exist.
    #[error("target not found: {0}")]
    TargetNotFound(String),
    /// The operation finished but some items failed.
    #[error("partial failure: {failed} of {total} items failed")]
    PartialFailure {
        /// Items that failed.
        failed: usize,
        /// Items attempted.
        total: usize,
    },
    /// The user answered "no" to a confirmation.
    #[error("aborted by user")]
    AbortedByUser,
    /// Confirmation was required but no TTY was available and `--yes` was absent.
    #[error("confirmation required: no TTY and --yes not given")]
    ConfirmationRequired,
    /// Unsupported macOS version or architecture.
    #[error("unsupported system: {0}")]
    UnsupportedSystem(String),
    /// Scan cache corrupted or unreadable.
    #[error("cache error: {0}")]
    Cache(String),
    /// Configuration file invalid.
    #[error("config error: {0}")]
    Config(String),
    /// I/O error with context.
    #[error("{context}: {source}")]
    Io {
        /// What Broza was doing.
        context: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// Any other error.
    #[error("{0}")]
    Other(String),
}

impl BrozaError {
    /// Classify an [`std::io::Error`] that occurred while working on `path`.
    ///
    /// `EACCES` and `EPERM` become [`BrozaError::PermissionDenied`] (exit `3`);
    /// every other kind becomes [`BrozaError::Io`] (exit `1`). `Config` is
    /// reserved for parse and validation failures and is never produced here.
    pub fn from_io(context: impl Into<String>, path: &std::path::Path, source: std::io::Error) -> Self {
        if source.kind() == std::io::ErrorKind::PermissionDenied {
            return Self::PermissionDenied { path: path.to_path_buf() };
        }
        Self::Io { context: format!("{} {}", context.into(), path.display()), source }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::io::{Error, ErrorKind};
    use std::path::Path;

    use super::*;
    use crate::ExitCode;

    #[test]
    fn permission_errors_become_exit_three() {
        let err = BrozaError::from_io(
            "reading",
            Path::new("/etc/broza.toml"),
            Error::from(ErrorKind::PermissionDenied),
        );
        assert!(matches!(err, BrozaError::PermissionDenied { .. }), "{err}");
        assert_eq!(ExitCode::from(&err), ExitCode::PermissionDenied);
    }

    #[test]
    fn other_io_errors_become_exit_one_and_name_the_path() {
        let err = BrozaError::from_io("reading", Path::new("/tmp/x.toml"), Error::from(ErrorKind::NotFound));
        assert!(matches!(err, BrozaError::Io { .. }), "{err}");
        assert_eq!(ExitCode::from(&err), ExitCode::GenericError);
        assert!(err.to_string().contains("/tmp/x.toml"), "{err}");
    }
}
