//! Minimal fakes for the safety-kernel integration test.
//!
//! Deliberately local: the shared fakes in `broza::testing` belong to another
//! milestone slice, and the safety kernel must be testable on its own.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use broza::BrozaError;
use broza::model::{Volume, VolumeRole};
use broza::ports::{Answer, ConfirmationRequest, EntryMetadata, FileOps, Prompter};
use broza::scan::{MountEntry, MountTable};

/// In-memory filesystem: a flat map from path to `lstat` metadata.
#[derive(Debug, Clone, Default)]
pub struct MemFs {
    entries: BTreeMap<PathBuf, EntryMetadata>,
}

impl MemFs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn dir(self, path: impl AsRef<Path>) -> Self {
        self.insert(path.as_ref(), metadata(true, false, 0))
    }

    pub fn file(self, path: impl AsRef<Path>, size_bytes: u64) -> Self {
        self.insert(path.as_ref(), metadata(false, false, size_bytes))
    }

    pub fn symlink(self, path: impl AsRef<Path>) -> Self {
        self.insert(path.as_ref(), metadata(false, true, 0))
    }

    fn insert(self, path: &Path, entry: EntryMetadata) -> Self {
        let mut entries = self.entries;
        for parent in path.ancestors().skip(1).filter(|p| *p != Path::new("")) {
            entries.entry(parent.to_path_buf()).or_insert_with(|| metadata(true, false, 0));
        }
        entries.insert(path.to_path_buf(), entry);
        Self { entries }
    }
}

fn metadata(is_dir: bool, is_symlink: bool, size_bytes: u64) -> EntryMetadata {
    EntryMetadata {
        device: 2,
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

fn unsupported(operation: &str) -> BrozaError {
    BrozaError::Other(format!("MemFs does not support `{operation}`"))
}

impl FileOps for MemFs {
    fn metadata(&self, path: &Path) -> Result<EntryMetadata, BrozaError> {
        self.entries.get(path).cloned().ok_or_else(|| BrozaError::TargetNotFound(path.display().to_string()))
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>, BrozaError> {
        Ok(self.entries.keys().filter(|entry| entry.parent() == Some(path)).cloned().collect())
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

/// A prompter that always gives the same answer and counts how often it is asked.
#[derive(Debug)]
pub struct FakePrompter {
    answer: Answer,
    asked: AtomicUsize,
    literal_asked: AtomicUsize,
}

impl FakePrompter {
    pub fn answering(answer: Answer) -> Self {
        Self { answer, asked: AtomicUsize::new(0), literal_asked: AtomicUsize::new(0) }
    }

    pub fn asked(&self) -> usize {
        self.asked.load(Ordering::Relaxed)
    }

    pub fn literal_asked(&self) -> usize {
        self.literal_asked.load(Ordering::Relaxed)
    }
}

impl Prompter for FakePrompter {
    fn confirm(&self, _request: &ConfirmationRequest) -> Answer {
        self.asked.fetch_add(1, Ordering::Relaxed);
        self.answer
    }

    fn confirm_literal(&self, _request: &ConfirmationRequest, _expected: &str) -> Answer {
        self.asked.fetch_add(1, Ordering::Relaxed);
        self.literal_asked.fetch_add(1, Ordering::Relaxed);
        self.answer
    }
}

fn volume(id: &str, role: VolumeRole, mount: &str) -> Volume {
    Volume {
        id: id.parse().unwrap_or_else(|error| panic!("{error}")),
        name: id.to_owned(),
        role,
        mount_point: Some(PathBuf::from(mount)),
        used_bytes: 0,
        writable_by_broza: role.writable_by_broza(),
        purpose: String::new(),
    }
}

/// A mount table shaped like a modern Mac: a sealed System volume, a Data volume
/// reachable through firmlinks, the VM volume and one external disk.
pub fn mount_table() -> MountTable {
    MountTable::new(vec![
        MountEntry {
            mount_point: PathBuf::from("/"),
            device: 1,
            volume: volume("disk3s1", VolumeRole::System, "/"),
            firmlinks: Vec::new(),
        },
        MountEntry {
            mount_point: PathBuf::from("/System/Volumes/Data"),
            device: 2,
            volume: volume("disk3s5", VolumeRole::Data, "/System/Volumes/Data"),
            firmlinks: vec![
                PathBuf::from("/Users"),
                PathBuf::from("/Applications"),
                PathBuf::from("/Library"),
            ],
        },
        MountEntry {
            mount_point: PathBuf::from("/System/Volumes/VM"),
            device: 3,
            volume: volume("disk3s6", VolumeRole::Vm, "/System/Volumes/VM"),
            firmlinks: Vec::new(),
        },
        MountEntry {
            mount_point: PathBuf::from("/Volumes/External"),
            device: 4,
            volume: volume("disk4s1", VolumeRole::User, "/Volumes/External"),
            firmlinks: Vec::new(),
        },
    ])
}
