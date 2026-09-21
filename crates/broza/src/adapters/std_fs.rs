//! Real filesystem adapter built on `std::fs` and `std::os::unix::fs::MetadataExt`.
//!
//! Everything Broza needs from `stat` is already exposed by [`MetadataExt`], so
//! the only `libc` calls here are `renamex_np` and `flock`: `std` wraps neither
//! the `RENAME_EXCL` flag that makes a rename refuse to replace its
//! destination, nor an advisory whole-file lock. Symlinks are never followed —
//! `metadata` is an `lstat` and `remove_tree` on a link removes the link, not
//! what it points at.
//!
//! # Filesystems that do not have `renamex_np`
//!
//! APFS and HFS+ implement it. exFAT and several network filesystems answer
//! `ENOTSUP`, and Broza then checks the destination itself and reports
//! [`RenameMode::CheckedFallback`] so the caller can warn that the move was not
//! atomic against a concurrent writer. The behaviour is pinned by a fake that
//! reproduces `ENOTSUP`
//! ([`FakeFileOps::deny_exclusive_rename`](crate::testing::FakeFileOps::deny_exclusive_rename));
//! a real exFAT volume cannot be mounted from a test, so that path is verified
//! against the documented errno rather than against hardware.

use std::ffi::CString;
use std::fs::{self, Permissions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::macos::fs::MetadataExt as MacMetadataExt;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rayon::iter::{IntoParallelIterator, ParallelIterator};

use crate::BrozaError;
use crate::adapters::io_error::from_io;
use crate::ports::{DirListing, EntryMetadata, FileOps, FsLock, RenameMode};

/// Size of the blocks `st_blocks` counts, fixed at 512 bytes by POSIX.
const STAT_BLOCK_BYTES: u64 = 512;
/// Mode a file created by [`FileOps::write_atomic`] gets: owner writes, all read.
const DEFAULT_FILE_MODE: u32 = 0o644;
/// The permission bits of `st_mode`, without the file type.
const MODE_BITS: u32 = 0o7777;
/// `SF_DATALESS` in `st_flags`: a cloud placeholder whose contents are elsewhere.
const SF_DATALESS: u32 = 0x4000_0000;

/// [`FileOps`] backed by the real filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdFileOps;

impl FileOps for StdFileOps {
    fn metadata(&self, path: &Path) -> Result<EntryMetadata, BrozaError> {
        let meta = fs::symlink_metadata(path)
            .map_err(|source| from_io(format!("stat {}", path.display()), path, source))?;
        let kind = meta.file_type();
        Ok(EntryMetadata {
            device: meta.dev(),
            inode: meta.ino(),
            size_bytes: meta.size(),
            allocated_bytes: meta.blocks().saturating_mul(STAT_BLOCK_BYTES),
            link_count: meta.nlink(),
            is_dir: kind.is_dir(),
            is_symlink: kind.is_symlink(),
            is_dataless: meta.st_flags() & SF_DATALESS != 0,
            modified: timestamp(meta.mtime(), meta.mtime_nsec()),
            accessed: timestamp(meta.atime(), meta.atime_nsec()),
        })
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>, BrozaError> {
        let entries = fs::read_dir(path)
            .map_err(|source| from_io(format!("read directory {}", path.display()), path, source))?;
        let mut children = Vec::new();
        for entry in entries {
            let entry = entry
                .map_err(|source| from_io(format!("read directory {}", path.display()), path, source))?;
            children.push(entry.path());
        }
        Ok(children)
    }

    fn read_dir_with_metadata(&self, path: &Path) -> Result<DirListing, BrozaError> {
        if let Some(listing) = crate::adapters::bulk_dir::read_dir_with_attributes(path) {
            return Ok(listing);
        }
        // The kernel would not answer in bulk here: pair up `readdir` and
        // `lstat` like everybody else, but spread the `lstat`s over the pool.
        // They are independent, and on a cold cache each one waits on the disk
        // rather than on a core.
        let children = self.read_dir(path)?;
        Ok(children
            .into_par_iter()
            .map(|child| {
                let meta = self.metadata(&child);
                (child, meta)
            })
            .collect())
    }

    fn exists(&self, path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok()
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), BrozaError> {
        let context = format!("rename {} to {}", from.display(), to.display());
        fs::rename(from, to).map_err(|source| from_io(context, blame_rename(from, to, &source), source))
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), BrozaError> {
        fs::create_dir_all(path)
            .map_err(|source| from_io(format!("create directory {}", path.display()), path, source))
    }

    fn remove_tree(&self, path: &Path) -> Result<(), BrozaError> {
        let context = format!("remove {}", path.display());
        let meta =
            fs::symlink_metadata(path).map_err(|source| from_io(context.clone(), path, source))?.file_type();
        let removed = if meta.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) };
        removed.map_err(|source| from_io(context, path, source))
    }

    fn write_atomic(&self, path: &Path, contents: &[u8]) -> Result<(), BrozaError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let context = format!("write {}", path.display());
        let mut temp = tempfile::NamedTempFile::new_in(parent)
            .map_err(|source| from_io(context.clone(), parent, source))?;
        temp.write_all(contents).map_err(|source| from_io(context.clone(), path, source))?;
        let mode = Permissions::from_mode(existing_mode(path).unwrap_or(DEFAULT_FILE_MODE));
        temp.as_file().set_permissions(mode).map_err(|source| from_io(context.clone(), path, source))?;
        temp.as_file().sync_all().map_err(|source| from_io(context.clone(), path, source))?;
        temp.persist(path).map_err(|error| from_io(context, path, error.error))?;
        sync_directory(parent)
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>, BrozaError> {
        fs::read(path).map_err(|source| from_io(format!("read {}", path.display()), path, source))
    }

    fn create_dir_exclusive(&self, path: &Path) -> Result<(), BrozaError> {
        fs::create_dir(path)
            .map_err(|source| from_io(format!("create directory {}", path.display()), path, source))
    }

    fn rename_exclusive(&self, from: &Path, to: &Path) -> Result<RenameMode, BrozaError> {
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

    fn lock_exclusive(&self, path: &Path) -> Result<Box<dyn FsLock>, BrozaError> {
        let context = format!("lock {}", path.display());
        let file = open_lock_file(path).map_err(|source| from_io(context.clone(), path, source))?;
        // SAFETY: the descriptor is owned by `file`, which outlives the call and
        // is not closed until the returned guard is dropped. `flock` reads no
        // memory through it.
        #[allow(unsafe_code, reason = "std has no wrapper for flock")]
        let code = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if code == 0 {
            return Ok(Box::new(StdFsLock { _file: file }));
        }
        let failure = std::io::Error::last_os_error();
        if matches!(failure.raw_os_error(), Some(libc::EWOULDBLOCK)) {
            return Err(BrozaError::Io {
                context: format!("{context}: another Broza is working on it"),
                source: std::io::Error::from(std::io::ErrorKind::WouldBlock),
            });
        }
        Err(from_io(context, path, failure))
    }
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

/// Permission bits a path the caller already owns keeps through a rewrite.
///
/// `None` when the destination does not exist yet, or is not a plain file whose
/// mode is worth inheriting.
fn existing_mode(path: &Path) -> Option<u32> {
    let meta = fs::symlink_metadata(path).ok()?;
    meta.file_type().is_file().then(|| meta.permissions().mode() & MODE_BITS)
}

/// Name the path a failed rename is really about.
///
/// The kernel reports one errno for the whole operation, so a missing destination
/// directory arrives as `ENOENT` and would otherwise be blamed on the source.
fn blame_rename<'a>(from: &'a Path, to: &'a Path, source: &std::io::Error) -> &'a Path {
    if source.kind() != std::io::ErrorKind::NotFound || !path_exists(from) {
        return from;
    }
    to.parent().unwrap_or(to)
}

/// `true` when something is at `path`, symlinks not followed.
fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Flush the directory entry itself so the rename survives a crash.
fn sync_directory(path: &Path) -> Result<(), BrozaError> {
    let context = format!("sync directory {}", path.display());
    let dir = fs::File::open(path).map_err(|source| from_io(context.clone(), path, source))?;
    dir.sync_all().map_err(|source| from_io(context, path, source))
}

/// Convert a `stat` time into a [`Timestamp`], dropping values outside its range.
fn timestamp(seconds: i64, nanoseconds: i64) -> Option<Timestamp> {
    let nanoseconds = i32::try_from(nanoseconds).ok()?;
    Timestamp::new(seconds, nanoseconds).ok()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use std::os::unix::fs::PermissionsExt;

    use super::{DEFAULT_FILE_MODE, MODE_BITS, STAT_BLOCK_BYTES, StdFileOps, timestamp};
    use crate::BrozaError;
    use crate::ports::FileOps;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"))
    }

    fn mode_of(path: &Path) -> u32 {
        let meta = std::fs::symlink_metadata(path).unwrap_or_else(|e| panic!("{e}"));
        meta.permissions().mode() & MODE_BITS
    }

    #[test]
    fn allocated_size_counts_whole_blocks() {
        let dir = tempdir();
        let path = dir.path().join("file");
        StdFileOps.write_atomic(&path, &[0_u8; 100]).unwrap_or_else(|e| panic!("{e}"));

        let meta = StdFileOps.metadata(&path).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(meta.size_bytes, 100);
        assert!(meta.allocated_bytes >= STAT_BLOCK_BYTES, "{}", meta.allocated_bytes);
        assert_eq!(meta.allocated_bytes % STAT_BLOCK_BYTES, 0);
    }

    #[test]
    fn writing_into_a_missing_directory_reports_the_directory_as_not_found() {
        let dir = tempdir();
        let path = dir.path().join("missing/file");

        let err = StdFileOps.write_atomic(&path, b"x").err();

        assert!(matches!(err, Some(BrozaError::TargetNotFound(_))), "{err:?}");
    }

    #[test]
    fn writing_atomically_leaves_no_temporary_file_behind() {
        let dir = tempdir();
        let path = dir.path().join("file");

        StdFileOps.write_atomic(&path, b"x").unwrap_or_else(|e| panic!("{e}"));

        let children = StdFileOps.read_dir(dir.path()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(children, vec![path]);
    }

    #[test]
    fn creating_a_directory_twice_succeeds() {
        let dir = tempdir();
        let path = dir.path().join("a/b/c");

        StdFileOps.create_dir_all(&path).unwrap_or_else(|e| panic!("{e}"));
        StdFileOps.create_dir_all(&path).unwrap_or_else(|e| panic!("{e}"));

        assert!(StdFileOps.exists(&path));
    }

    #[test]
    fn a_directory_cannot_be_created_where_a_file_already_is() {
        let dir = tempdir();
        let path = dir.path().join("file");
        StdFileOps.write_atomic(&path, b"x").unwrap_or_else(|e| panic!("{e}"));

        let err = StdFileOps.create_dir_all(&path.join("child")).err();

        assert!(matches!(err, Some(BrozaError::Io { .. })), "{err:?}");
    }

    #[test]
    fn writing_over_a_directory_fails_and_leaves_the_directory_alone() {
        let dir = tempdir();
        let path = dir.path().join("subdir");
        StdFileOps.create_dir_all(&path).unwrap_or_else(|e| panic!("{e}"));

        let err = StdFileOps.write_atomic(&path, b"x").err();

        assert!(err.is_some(), "writing over a directory must fail");
        assert!(StdFileOps.metadata(&path).is_ok_and(|meta| meta.is_dir));
    }

    #[test]
    fn renaming_a_missing_entry_blames_the_source() {
        let dir = tempdir();
        let from = dir.path().join("a");

        let err = StdFileOps.rename(&from, &dir.path().join("b")).err();

        let Some(BrozaError::TargetNotFound(blamed)) = err else { panic!("{err:?}") };
        assert_eq!(blamed, from.display().to_string());
    }

    #[test]
    fn renaming_into_a_missing_directory_blames_that_directory() {
        let dir = tempdir();
        let from = dir.path().join("a");
        StdFileOps.write_atomic(&from, b"x").unwrap_or_else(|e| panic!("{e}"));

        let err = StdFileOps.rename(&from, &dir.path().join("missing/a")).err();

        let Some(BrozaError::TargetNotFound(blamed)) = err else { panic!("{err:?}") };
        assert_eq!(blamed, dir.path().join("missing").display().to_string());
    }

    #[test]
    fn a_new_file_is_readable_by_everyone_and_writable_by_its_owner() {
        let dir = tempdir();
        let path = dir.path().join("fresh");

        StdFileOps.write_atomic(&path, b"x").unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(mode_of(&path), DEFAULT_FILE_MODE);
    }

    #[test]
    fn rewriting_a_file_keeps_the_mode_it_had() {
        let dir = tempdir();
        let path = dir.path().join("private");
        StdFileOps.write_atomic(&path, b"secret").unwrap_or_else(|e| panic!("{e}"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .unwrap_or_else(|e| panic!("{e}"));

        StdFileOps.write_atomic(&path, b"still secret").unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(mode_of(&path), 0o600);
    }

    #[test]
    fn the_fallback_for_a_filesystem_without_renamex_np_moves_the_file() {
        let dir = tempdir();
        let (from, to) = (dir.path().join("from"), dir.path().join("to"));
        StdFileOps.write_atomic(&from, b"content").unwrap_or_else(|e| panic!("{e}"));

        let mode = super::checked_rename(&from, &to, "rename".to_owned());

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

        let refused = super::checked_rename(&from, &to, "rename".to_owned());

        assert!(refused.is_err_and(|error| crate::ports::already_exists(&error)));
        assert_eq!(StdFileOps.read(&to).ok(), Some(b"old".to_vec()));
    }

    #[test]
    fn the_fallback_reports_a_missing_source_like_any_other_rename() {
        let dir = tempdir();
        let from = dir.path().join("ghost");

        let error = super::checked_rename(&from, &dir.path().join("to"), "rename".to_owned()).err();

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

    #[test]
    fn a_stat_time_out_of_range_is_reported_as_unknown() {
        assert!(timestamp(0, 0).is_some());
        assert!(timestamp(i64::MAX, 0).is_none());
        assert!(timestamp(0, i64::from(i32::MAX) + 1).is_none());
    }

    #[test]
    fn reading_a_directory_as_a_file_fails_without_panicking() {
        let dir = tempdir();

        let err = StdFileOps.read(dir.path()).err();

        assert!(err.is_some());
        assert!(!StdFileOps.exists(Path::new("/definitely/not/here")));
    }
}
