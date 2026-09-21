//! One mapping from [`std::io::Error`] to [`BrozaError`], shared by the real
//! filesystem adapter and the in-memory fake so both report the same error for the
//! same situation.

use std::io;
use std::path::Path;

use crate::BrozaError;

/// Translate an I/O failure on `path` into the core error type.
///
/// The kind decides the exit code (`docs/cli-spec.md` §5): a missing path is
/// `TARGET_NOT_FOUND`, a refused one is `PERMISSION_DENIED`, everything else keeps
/// the underlying error and `context` for the message.
pub(crate) fn from_io(context: impl Into<String>, path: &Path, source: io::Error) -> BrozaError {
    match source.kind() {
        io::ErrorKind::NotFound => BrozaError::TargetNotFound(path.display().to_string()),
        io::ErrorKind::PermissionDenied => BrozaError::PermissionDenied { path: path.to_path_buf() },
        _ => BrozaError::Io { context: context.into(), source },
    }
}

/// Build the error a missing `path` produces, without an underlying failure.
///
/// Only the in-memory fake needs this: the real adapter always has an
/// [`std::io::Error`] to translate.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn not_found(path: &Path) -> BrozaError {
    BrozaError::TargetNotFound(path.display().to_string())
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::Path;

    use super::{from_io, not_found};
    use crate::BrozaError;

    #[test]
    fn a_missing_path_becomes_a_target_not_found_error() {
        let err = from_io("stat", Path::new("/a"), io::Error::from(io::ErrorKind::NotFound));

        assert!(matches!(err, BrozaError::TargetNotFound(path) if path == "/a"));
    }

    #[test]
    fn a_refused_path_becomes_a_permission_error_carrying_the_path() {
        let err = from_io("stat", Path::new("/a"), io::Error::from(io::ErrorKind::PermissionDenied));

        assert!(matches!(err, BrozaError::PermissionDenied { path } if path == Path::new("/a")));
    }

    #[test]
    fn any_other_failure_keeps_its_context_and_source() {
        let err = from_io("stat /a", Path::new("/a"), io::Error::from_raw_os_error(18));

        let BrozaError::Io { context, source } = err else { panic!("expected BrozaError::Io") };
        assert_eq!(context, "stat /a");
        assert_eq!(source.raw_os_error(), Some(18));
    }

    #[test]
    fn a_path_known_to_be_missing_needs_no_underlying_error() {
        assert!(matches!(not_found(Path::new("/b")), BrozaError::TargetNotFound(path) if path == "/b"));
    }
}
