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
//! grep -rn "fn rename\|fn remove_tree\|fn write_atomic\|fn create_dir_all" crates/
//! ```
//!
//! Only `clean::executor` and `quarantine::{mover,restore,expiry}` may call those
//! four methods **on the user's data**, and only while holding an
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

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::BrozaError;

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
