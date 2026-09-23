//! Filesystem access.
//!
//! Every mutating method exists so that the safety kernel can gate it: callers outside
//! `clean/` and `quarantine/` must not need them.
//!
//! # Second audit surface
//!
//! `Approved<_>` proves that a write was *authorised*; this trait is where a write
//! actually happens, so it is the other list a reviewer has to read:
//!
//! ```text
//! grep -rn "fn rename\|fn remove_tree\|fn write_atomic\|fn create_dir" crates/
//! ```
//!
//! Only `clean::executor` and `quarantine::*` may call the mutating methods
//! (`rename`, `rename_exclusive`, `create_dir_all`, `create_dir_exclusive`,
//! `remove_tree`, `write_atomic`), and only while holding an
//! [`Approved`](crate::safety::guard::Approved) token whose
//! [`ApprovedItem`](crate::safety::guard::ApprovedItem) covers the path — after
//! re-`lstat`ing it and comparing `(device, inode)`. Any other call site is a bug,
//! and review rejects it; the trait is deliberately not split, because splitting it
//! would only move the obligation somewhere less visible.
//!
//! One exception exists, and it is not about user data: `scan::cache::store` calls
//! `create_dir_all` and `write_atomic` on Broza's own cache file under
//! `~/.cache/broza/v1/` (`docs/cli-spec.md` §7). It never touches a path the user
//! asked about, never deletes anything, and a lost cache costs one slow scan — so
//! it needs no `Approved` token. Any *other* writer outside the list above is a bug.

use std::any::Any;
use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::BrozaError;

/// An open directory, held while its children are being listed.
///
/// The walker lists by handle rather than by path: a path of `PATH_MAX`
/// (1024) bytes or more cannot be opened or stated at all, while a directory
/// can be created that deep with short relative names. The adapter decides
/// what a handle is and how it reaches a directory that deep — a descriptor
/// for the real filesystem, the path itself for the in-memory one.
/// [`DirHandle::as_any`] lets the adapter that made a handle get it back.
pub trait DirHandle: Send + Sync {
    /// The handle as `Any`, so its adapter can downcast it.
    fn as_any(&self) -> &dyn Any;
}

/// The handle the path-based defaults hand out: the directory's own path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathDirHandle(pub PathBuf);

impl DirHandle for PathDirHandle {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// One directory listing: every child with the metadata read for it.
///
/// A child that could not be stated carries the error instead of the metadata,
/// so the caller can warn about it rather than lose it.
pub type DirListing = Vec<(PathBuf, Result<EntryMetadata, BrozaError>)>;

/// Metadata of a filesystem entry, obtained without following symlinks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryMetadata {
    /// Device id (`st_dev`). Two paths with different devices are on different volumes.
    pub device: u64,
    /// Inode number (`st_ino`).
    pub inode: u64,
    /// Apparent size in bytes.
    pub size_bytes: u64,
    /// Allocated size in bytes (`st_blocks * 512`).
    pub allocated_bytes: u64,
    /// Number of hard links.
    pub link_count: u64,
    /// `true` for directories.
    pub is_dir: bool,
    /// `true` for symbolic links.
    pub is_symlink: bool,
    /// `true` when the entry is a cloud placeholder whose contents are not on
    /// this disk (`SF_DATALESS`: iCloud Drive, Files On-Demand providers).
    ///
    /// Its `size_bytes` is what the file *would* take once downloaded, so a
    /// scan that counted it would report space that is not in use. Touching one
    /// is also expensive: opening or listing it blocks on the provider.
    pub is_dataless: bool,
    /// Last modification time.
    pub modified: Option<Timestamp>,
    /// Last access time.
    pub accessed: Option<Timestamp>,
    /// The APFS clone id of a regular file (`ATTR_CMNEXT_CLONEID`).
    ///
    /// Files that were cloned from one another share it; a file that was
    /// never cloned carries its own inode number. `None` for directories,
    /// symlinks, and filesystems that have no such notion, so a caller can
    /// tell "not a clone" from "cannot know". See [`EntryMetadata::is_clone`].
    pub clone_id: Option<u64>,
}

impl EntryMetadata {
    /// `true` when this file shares its blocks with the file it was cloned from.
    ///
    /// The original keeps a clone id equal to its inode, so it reads as an
    /// ordinary file here; only the copies answer `true`. Deciding which of
    /// them holds the bytes is the walker's job (`scan::walker::clones`).
    pub fn is_clone(&self) -> bool {
        self.clone_id.is_some_and(|id| id != self.inode)
    }
}

/// The BLAKE3 hash of a file's whole contents.
///
/// Two files with the same hash hold the same bytes, for every purpose Broza
/// has: the duplicates detector compares these, never the files again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash(pub [u8; 32]);

/// Filesystem reads and writes.
///
/// # Every answer is already out of date
///
/// This port is path-based, so every call is a fresh lookup and nothing here is
/// atomic with anything else. Between a [`FileOps::metadata`] that says "a regular
/// file on device 2" and the [`FileOps::rename`] acting on it, the path can become a
/// symlink pointing anywhere, or a different file altogether. Checking first and
/// acting later is a time-of-check to time-of-use gap, and for a tool that deletes
/// things that gap is the whole attack.
///
/// Reading is allowed to live with it. Mutating is not: the quarantine mover (M3)
/// must re-verify the `(device, inode)` pair it recorded **after** the rename and
/// roll back when it does not match, rather than trusting the metadata it read
/// while planning. A future revision may add handle-based operations
/// (`openat`/`renameat`) to close the gap for good; until then the re-check is the
/// contract.
pub trait FileOps: Send + Sync {
    /// `lstat`: metadata without following the final symlink.
    fn metadata(&self, path: &Path) -> Result<EntryMetadata, BrozaError>;
    /// Direct children of a directory (names joined to `path`).
    fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>, BrozaError>;
    /// Direct children of a directory, each with the metadata of a `lstat`.
    ///
    /// The walker asks for a whole directory at once because the alternative —
    /// one `readdir` plus one `lstat` per entry — is two syscalls per file, and
    /// a scan looks at millions of them. The default implementation is exactly
    /// that pair, so an adapter only overrides this when its platform offers
    /// something better (macOS: `getattrlistbulk`).
    ///
    /// A child whose metadata cannot be read keeps its place in the listing
    /// with the error: a directory the process may not stat is something the
    /// user has to be told about, not something to drop quietly.
    fn read_dir_with_metadata(&self, path: &Path) -> Result<DirListing, BrozaError> {
        Ok(self
            .read_dir(path)?
            .into_iter()
            .map(|child| {
                let meta = self.metadata(&child);
                (child, meta)
            })
            .collect())
    }
    /// Open the directory at `path`, however deep, so its children can be
    /// listed ([`FileOps::list_dir`]).
    ///
    /// The default is path-based and never fails here: whatever is wrong with
    /// the path surfaces when the directory is listed.
    fn open_dir(&self, path: &Path) -> Result<Box<dyn DirHandle>, BrozaError> {
        Ok(Box::new(PathDirHandle(path.to_path_buf())))
    }
    /// Direct children of the open directory `dir`, each with the metadata of
    /// a `lstat`, their names joined to `path`.
    ///
    /// The default is [`FileOps::read_dir_with_metadata`] on the path.
    fn list_dir(&self, dir: &dyn DirHandle, path: &Path) -> Result<DirListing, BrozaError> {
        let _ = dir;
        self.read_dir_with_metadata(path)
    }
    /// `true` when something exists at `path` (symlinks are not followed).
    fn exists(&self, path: &Path) -> bool;
    /// Atomic rename within one device.
    fn rename(&self, from: &Path, to: &Path) -> Result<(), BrozaError>;
    /// Create a directory and all missing parents.
    fn create_dir_all(&self, path: &Path) -> Result<(), BrozaError>;
    /// Remove a file or a whole directory tree. Irreversible.
    fn remove_tree(&self, path: &Path) -> Result<(), BrozaError>;
    /// Write a whole file atomically (temp file + fsync + rename).
    fn write_atomic(&self, path: &Path, contents: &[u8]) -> Result<(), BrozaError>;
    /// Read a whole file.
    fn read(&self, path: &Path) -> Result<Vec<u8>, BrozaError>;
    /// `len` bytes of a file from `offset`; fewer when the file ends first.
    ///
    /// The default reads the whole file and keeps the slice, which is what a
    /// fake can afford; the real adapter seeks and reads only that much.
    fn read_range(&self, path: &Path, offset: u64, len: usize) -> Result<Vec<u8>, BrozaError> {
        let bytes = self.read(path)?;
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(bytes.len());
        let end = start.saturating_add(len).min(bytes.len());
        Ok(bytes[start..end].to_vec())
    }
    /// The [`ContentHash`] of a whole file.
    ///
    /// The default hashes what [`FileOps::read`] returns; the real adapter
    /// streams the file through the hasher instead of holding it in memory.
    fn hash_file(&self, path: &Path) -> Result<ContentHash, BrozaError> {
        Ok(ContentHash(*blake3::hash(&self.read(path)?).as_bytes()))
    }
    /// Create one directory, failing when something is already at `path`.
    ///
    /// `mkdir(2)`: the parent must exist, and an existing `path` is refused with
    /// `EEXIST`, which [`already_exists`] recognises. Claiming a name this way is
    /// how the quarantine mover makes a session identifier unique — two runs that
    /// derive the same name in the same second cannot both succeed.
    fn create_dir_exclusive(&self, path: &Path) -> Result<(), BrozaError>;
    /// Rename, refusing to replace anything already at `to`.
    ///
    /// `renamex_np(2)` with `RENAME_EXCL`. Plain [`FileOps::rename`] silently
    /// replaces the destination, and for a tool that moves user data that is the
    /// difference between quarantining a file and destroying one, so every
    /// rename inside `quarantine/` uses this instead. Renaming a path onto
    /// itself succeeds and moves nothing, as it does on APFS.
    ///
    /// Not every filesystem has the call; see [`RenameMode`] for what a caller
    /// has to say when it did not.
    fn rename_exclusive(&self, from: &Path, to: &Path) -> Result<RenameMode, BrozaError>;
    /// Take an exclusive lock on `path`, without waiting for it.
    ///
    /// `flock(2)` with `LOCK_EX | LOCK_NB` on a lock file the call creates if it
    /// is missing. The lock lives as long as the returned value and is released
    /// when it is dropped. A lock another process holds is reported as an error
    /// [`is_busy`] recognises, never waited for: Broza would rather tell the
    /// user that a cleanup is running than block a terminal indefinitely.
    fn lock_exclusive(&self, path: &Path) -> Result<Box<dyn FsLock>, BrozaError>;
}

/// An exclusive lock, held until this value is dropped.
///
/// Deliberately empty: a lock is a *fact for a span of time*, not an object
/// with behaviour. `Send` so that it can be held across the whole of an
/// operation the caller may have moved onto another thread.
pub trait FsLock: Send {}

/// How [`FileOps::rename_exclusive`] managed to avoid replacing the destination.
///
/// APFS and HFS+ implement `renamex_np`; exFAT and some network filesystems
/// answer `ENOTSUP`, and Broza then does the check itself. The two are not
/// equivalent and the caller has to say so: the fallback has a window between
/// the check and the rename that the kernel call does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameMode {
    /// The kernel refused to replace anything, atomically.
    Exclusive,
    /// The filesystem has no such call, so the destination was checked first.
    CheckedFallback,
}

/// `true` when `error` reports that someone else holds the lock.
///
/// The one error [`FileOps::lock_exclusive`] produces that is not a failure:
/// another Broza is working on that session, and the caller skips it rather
/// than touching what a running process is in the middle of moving.
pub fn is_busy(error: &BrozaError) -> bool {
    match error {
        BrozaError::Io { source, .. } => source.kind() == std::io::ErrorKind::WouldBlock,
        _ => false,
    }
}

/// `true` when `error` reports that the destination was already taken.
///
/// [`FileOps::create_dir_exclusive`] and [`FileOps::rename_exclusive`] are the
/// two calls that can produce it, and both callers need to tell "the name is
/// taken" apart from "the filesystem failed".
pub fn already_exists(error: &BrozaError) -> bool {
    match error {
        BrozaError::Io { source, .. } => source.kind() == std::io::ErrorKind::AlreadyExists,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::already_exists;
    use crate::BrozaError;

    #[test]
    fn only_a_lock_someone_else_holds_is_busy() {
        let held = BrozaError::Io {
            context: "lock".to_owned(),
            source: std::io::Error::from(std::io::ErrorKind::WouldBlock),
        };

        assert!(super::is_busy(&held));
        assert!(!super::is_busy(&BrozaError::TargetNotFound("/x".to_owned())));
        assert!(!super::is_busy(&BrozaError::Io {
            context: "lock".to_owned(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        }));
    }

    #[test]
    fn only_an_occupied_destination_is_an_already_exists() {
        let taken = BrozaError::Io {
            context: "create".to_owned(),
            source: std::io::Error::from(std::io::ErrorKind::AlreadyExists),
        };
        let other = BrozaError::Io {
            context: "create".to_owned(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };

        assert!(already_exists(&taken));
        assert!(!already_exists(&other));
        assert!(!already_exists(&BrozaError::TargetNotFound("/x".to_owned())));
    }
}
