//! The exclusive filesystem operations of [`StdFileOps`](super::std_fs::StdFileOps).
//!
//! Two `libc` calls `std` has no wrapper for: `renamex_np` with `RENAME_EXCL`,
//! so a quarantine move or restore never replaces what is already at the
//! destination, and `flock`, so two Brozas never work on one session at once.
//! Both are the only `unsafe` in this adapter and carry their SAFETY notes.

use std::ffi::CString;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::BrozaError;
use crate::adapters::io_error::from_io;
use crate::adapters::std_fs::{blame_rename, path_exists};
use crate::ports::{FsLock, RenameMode};

/// `renamex_np(RENAME_EXCL)`: rename without ever replacing an existing destination.
///
/// exFAT answers `ENOTSUP`; see [`checked_rename`] for what happens then.
pub(super) fn rename_exclusive(from: &Path, to: &Path) -> Result<RenameMode, BrozaError> {
    let context = format!("rename {} to {} without replacing it", from.display(), to.display());
    let source = c_path(from, &context)?;
    let destination = c_path(to, &context)?;
    // SAFETY: both pointers come from `CString`s that live until the end of
    // this statement, and `renamex_np` only reads them. `RENAME_EXCL` is the
    // documented flag that makes the call fail with `EEXIST` instead of
    // replacing an existing destination.
    #[allow(unsafe_code, reason = "std has no wrapper for renamex_np's RENAME_EXCL flag")]
    let code = unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    if code == 0 {
        return Ok(RenameMode::Exclusive);
    }
    let failure = std::io::Error::last_os_error();
    if failure.raw_os_error() == Some(libc::ENOTSUP) {
        return checked_rename(from, to, context);
    }
    Err(from_io(context, blame_rename(from, to, &failure), failure))
}

/// How many times a contended `flock` is retried, and how long between tries.
///
/// On macOS a lock released by `close` is, very rarely, still reported busy to a
/// `flock` issued a few microseconds later in the same process (observed about
/// once in fifteen full test runs). A handful of millisecond retries turns that
/// race into a non-event without changing what "busy" means: a lock somebody
/// else really holds is still busy after ten milliseconds.
const CONTENDED_RETRIES: u32 = 5;
const RETRY_PAUSE: std::time::Duration = std::time::Duration::from_millis(2);

/// An exclusive, non-blocking `flock` on `path`, released when the guard drops.
pub(super) fn lock_exclusive(path: &Path) -> Result<Box<dyn FsLock>, BrozaError> {
    let context = format!("lock {}", path.display());
    let file = open_lock_file(path).map_err(|source| from_io(context.clone(), path, source))?;
    let mut failure = try_flock(&file);
    for _ in 0..CONTENDED_RETRIES {
        match failure {
            Some(ref error) if error.raw_os_error() == Some(libc::EWOULDBLOCK) => {
                std::thread::sleep(RETRY_PAUSE);
                failure = try_flock(&file);
            }
            _ => break,
        }
    }
    let Some(failure) = failure else {
        return Ok(Box::new(StdFsLock { _file: file }));
    };
    if matches!(failure.raw_os_error(), Some(libc::EWOULDBLOCK)) {
        return Err(BrozaError::Io {
            context: format!("{context}: another Broza is working on it"),
            source: std::io::Error::from(std::io::ErrorKind::WouldBlock),
        });
    }
    Err(from_io(context, path, failure))
}

/// One non-blocking `flock` attempt; `None` when the lock was taken.
fn try_flock(file: &fs::File) -> Option<std::io::Error> {
    // SAFETY: the descriptor is owned by `file`, which outlives the call and
    // is not closed until the returned guard is dropped. `flock` reads no
    // memory through it.
    #[allow(unsafe_code, reason = "std has no wrapper for flock")]
    let code = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    (code != 0).then(std::io::Error::last_os_error)
}

/// Open a lock file for `flock`, which needs no write permission.
///
/// An existing file is opened read-only, so a `.lock` owned by somebody else
/// (left behind by `sudo broza`) can still be taken or found busy. Only a
/// missing file is created.
fn open_lock_file(path: &Path) -> std::io::Result<fs::File> {
    match fs::OpenOptions::new().read(true).open(path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
        }
        Err(error) => Err(error),
    }
}

/// An exclusive `flock`, released when the descriptor closes.
struct StdFsLock {
    /// Never read: closing it is what releases the lock.
    _file: fs::File,
}

impl FsLock for StdFsLock {}

/// The `renamex_np` fallback for a filesystem that does not implement it.
///
/// exFAT and several network filesystems answer `ENOTSUP`. Checking the
/// destination and renaming is the best a program can do there; the window
/// between the two is real, which is why the caller is told
/// ([`RenameMode::CheckedFallback`]) instead of being left to assume the
/// kernel guaranteed something it did not.
fn checked_rename(from: &Path, to: &Path, context: String) -> Result<RenameMode, BrozaError> {
    if from != to && path_exists(to) {
        return Err(BrozaError::Io {
            context,
            source: std::io::Error::from(std::io::ErrorKind::AlreadyExists),
        });
    }
    fs::rename(from, to)
        .map(|()| RenameMode::CheckedFallback)
        .map_err(|source| from_io(context, blame_rename(from, to, &source), source))
}

/// The path as a NUL-terminated C string, for the one call that needs `libc`.
fn c_path(path: &Path, context: &str) -> Result<CString, BrozaError> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| BrozaError::Io {
        context: context.to_owned(),
        source: std::io::Error::from(std::io::ErrorKind::InvalidInput),
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::checked_rename;
    use crate::BrozaError;
    use crate::adapters::StdFileOps;
    use crate::ports::FileOps;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"))
    }
    #[test]
    fn the_fallback_for_a_filesystem_without_renamex_np_moves_the_file() {
        let dir = tempdir();
        let (from, to) = (dir.path().join("from"), dir.path().join("to"));
        StdFileOps.write_atomic(&from, b"content").unwrap_or_else(|e| panic!("{e}"));

        let mode = checked_rename(&from, &to, "rename".to_owned());

        assert_eq!(mode.ok(), Some(crate::ports::RenameMode::CheckedFallback));
        assert_eq!(StdFileOps.read(&to).ok(), Some(b"content".to_vec()));
        assert!(!StdFileOps.exists(&from));
    }

    #[test]
    fn the_fallback_refuses_an_occupied_destination_instead_of_replacing_it() {
        let dir = tempdir();
        let (from, to) = (dir.path().join("from"), dir.path().join("to"));
        StdFileOps.write_atomic(&from, b"new").unwrap_or_else(|e| panic!("{e}"));
        StdFileOps.write_atomic(&to, b"old").unwrap_or_else(|e| panic!("{e}"));

        let refused = checked_rename(&from, &to, "rename".to_owned());

        assert!(refused.is_err_and(|error| crate::ports::already_exists(&error)));
        assert_eq!(StdFileOps.read(&to).ok(), Some(b"old".to_vec()));
    }

    #[test]
    fn the_fallback_reports_a_missing_source_like_any_other_rename() {
        let dir = tempdir();
        let from = dir.path().join("ghost");

        let error = checked_rename(&from, &dir.path().join("to"), "rename".to_owned()).err();

        assert!(matches!(error, Some(BrozaError::TargetNotFound(_))), "{error:?}");
    }

    #[test]
    fn a_lock_on_a_path_that_cannot_be_opened_is_a_plain_failure() {
        let dir = tempdir();

        let error = StdFileOps.lock_exclusive(&dir.path().join("missing/dir/.lock")).err();

        assert!(matches!(error, Some(BrozaError::TargetNotFound(_))), "{error:?}");
        assert!(!error.is_some_and(|failure| crate::ports::is_busy(&failure)), "not a busy lock");
    }

    #[test]
    fn a_path_with_an_interior_nul_is_refused_before_it_reaches_the_kernel() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempdir();
        let poisoned = Path::new(OsStr::from_bytes(b"/tmp/a\0b")).to_path_buf();

        let error = StdFileOps.rename_exclusive(&dir.path().join("a"), &poisoned).err();

        assert!(matches!(error, Some(BrozaError::Io { .. })), "{error:?}");
    }
}
