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
