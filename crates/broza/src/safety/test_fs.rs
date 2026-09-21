//! In-memory [`FileOps`] used by the unit tests of the safety kernel.
//!
//! The shared fakes live in `testing/` and are not available to this module's unit
//! tests without pulling the whole crate's test surface in; this is the smallest
//! thing that satisfies `lstat`-style lookups.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::ports::{EntryMetadata, FileOps};

/// Default device id of the fake tree.
pub(crate) const DEFAULT_DEVICE: u64 = 1;

/// A flat map from path to metadata; only `metadata` and `exists` are supported.
#[derive(Debug, Clone, Default)]
pub(crate) struct MemFs {
    entries: BTreeMap<PathBuf, EntryMetadata>,
}

impl MemFs {
    /// Empty tree.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Adds a directory and every missing parent.
    pub(crate) fn dir(self, path: impl AsRef<Path>) -> Self {
        self.insert(path.as_ref(), entry(true, false, 0))
    }

    /// Adds a file of `size_bytes` and every missing parent directory.
    pub(crate) fn file(self, path: impl AsRef<Path>, size_bytes: u64) -> Self {
        self.insert(path.as_ref(), entry(false, false, size_bytes))
    }

    /// Adds a symbolic link and every missing parent directory.
    pub(crate) fn symlink(self, path: impl AsRef<Path>) -> Self {
        self.insert(path.as_ref(), entry(false, true, 0))
    }

    fn insert(self, path: &Path, metadata: EntryMetadata) -> Self {
        let mut entries = self.entries;
        for parent in path.ancestors().skip(1).filter(|p| *p != Path::new("")) {
            entries.entry(parent.to_path_buf()).or_insert_with(|| entry(true, false, 0));
        }
        entries.insert(path.to_path_buf(), metadata);
        Self { entries }
    }
}

fn entry(is_dir: bool, is_symlink: bool, size_bytes: u64) -> EntryMetadata {
    EntryMetadata {
        device: DEFAULT_DEVICE,
        inode: 1,
        size_bytes,
        allocated_bytes: size_bytes,
        link_count: 1,
        is_dir,
        is_symlink,
        modified: None,
        accessed: None,
    }
}

impl FileOps for MemFs {
    fn metadata(&self, path: &Path) -> Result<EntryMetadata, BrozaError> {
        self.entries.get(path).cloned().ok_or_else(|| BrozaError::TargetNotFound(path.display().to_string()))
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>, BrozaError> {
        Ok(self.entries.keys().filter(|candidate| candidate.parent() == Some(path)).cloned().collect())
    }

    fn exists(&self, path: &Path) -> bool {
        self.entries.contains_key(path)
    }

    fn rename(&self, _from: &Path, _to: &Path) -> Result<(), BrozaError> {
        Err(unsupported("rename"))
    }

    fn create_dir_all(&self, _path: &Path) -> Result<(), BrozaError> {
        Err(unsupported("create_dir_all"))
    }

    fn remove_tree(&self, _path: &Path) -> Result<(), BrozaError> {
        Err(unsupported("remove_tree"))
    }

    fn write_atomic(&self, _path: &Path, _contents: &[u8]) -> Result<(), BrozaError> {
        Err(unsupported("write_atomic"))
    }

    fn read(&self, _path: &Path) -> Result<Vec<u8>, BrozaError> {
        Err(unsupported("read"))
    }
}

fn unsupported(operation: &str) -> BrozaError {
    BrozaError::Other(format!("MemFs does not support `{operation}`"))
}
