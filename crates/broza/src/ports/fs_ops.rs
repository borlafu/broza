//! Filesystem access.
//!
//! Every mutating method exists so that the safety kernel can gate it: callers outside
//! `clean/` and `quarantine/` must not need them.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::BrozaError;

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
    /// Last modification time.
    pub modified: Option<Timestamp>,
    /// Last access time.
    pub accessed: Option<Timestamp>,
}

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
}
