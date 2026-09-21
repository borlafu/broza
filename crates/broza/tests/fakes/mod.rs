//! Minimal fakes and the shared scenario for the safety-kernel integration tests.
//!
//! Deliberately local: the shared fakes in `broza::testing` belong to another
//! milestone slice, and the safety kernel must be testable on its own.
//!
//! Two test binaries include this module and each uses part of it.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use broza::clean::{PlanOutcome, Selection, plan_dry_run};
use broza::model::{Category, Finding, FindingPath, SessionId, Volume, VolumeRole};
use broza::ports::{Answer, ConfirmationRequest, EntryMetadata, FileOps, Prompter};
use broza::safety::guard::{Verdict, WriteRequest, approve};
use broza::safety::rejection::GuardRejection;
use broza::scan::{MountEntry, MountTable};
use broza::{BrozaError, ExitCode};

/// The home directory of the fake user.
pub const HOME: &str = "/Users/dana";
/// A cache file inside that home.
pub const CACHE: &str = "/Users/dana/Library/Caches/app.cache";
/// A second cache file, for size arithmetic.
pub const OTHER: &str = "/Users/dana/Library/Caches/other.cache";
/// The quarantine store of that home.
pub const STORE: &str = "/Users/dana/.local/share/broza/quarantine";

/// A fixed session id, so failures are reproducible.
pub fn session() -> SessionId {
    "cln_20260921103608_a1b2".parse().unwrap_or_else(|error| panic!("{error}"))
}

/// A finding with the given paths and sizes.
pub fn finding(id: &str, category: Category, paths: &[(&str, u64)]) -> Finding {
    Finding::builder(id.parse().unwrap_or_else(|e| panic!("{e}")), category, "title")
        .paths(
            paths
                .iter()
                .map(|(path, size)| FindingPath {
                    path: PathBuf::from(path),
                    size_bytes: *size,
                    last_used: None,
                })
                .collect(),
        )
        .reclaimable_bytes(paths.iter().map(|(_, size)| *size).fold(0_u64, u64::saturating_add))
        .build()
        .unwrap_or_else(|error| panic!("{error}"))
}

/// One green `user-cache` finding covering the given paths.
pub fn caches(paths: &[(&str, u64)]) -> Vec<Finding> {
    vec![finding("user-cache.app", Category::UserCache, paths)]
}

/// The planner's outcome for a selection, or a panic with its error.
pub fn planned(findings: &[Finding], selection: &Selection) -> PlanOutcome {
    plan_dry_run(findings, selection, session(), None).unwrap_or_else(|error| panic!("{error}"))
}

/// A request that applies the plan on an interactive terminal.
pub fn applying() -> WriteRequest {
    WriteRequest { apply: true, tty: true, ..WriteRequest::new(HOME) }
}

/// The filesystem every safety-kernel test runs against.
pub fn fs() -> MemFs {
    MemFs::new()
        .file(CACHE, 10)
        .file(OTHER, 20)
        .file("/System/Library/Caches/system.cache", 30)
        .file("/System/Volumes/VM/swapfile0", 40)
        .file("/System/Volumes/Data/Users/dana/Library/Caches/twin.cache", 10)
        .file("/Users/dana/Documents/report.pdf", 50)
        .file("/Users/other/Documents/secret.txt", 1)
        .symlink("/Users/dana/Library/Caches/linked")
        .symlink("/Users/dana/Library/evil")
        .file("/Volumes/External/.Trashes/501/old.dmg", 60)
        .file("/System/Volumes/Data/private/var/db/.Trashes/victim", 70)
        .file("/Users/dana/Library/Mobile Documents/synced.key", 70)
        .dir(STORE)
        .file("/Users/dana/.local/share/broza/quarantine/cln_20260921103608_a1b2/items/1/a", 5)
}

/// Plans and approves the given cache paths.
pub fn approve_paths(paths: &[(&str, u64)], req: &WriteRequest) -> Result<Verdict, GuardRejection> {
    let findings = caches(paths);
    let plan = planned(&findings, &Selection::everything()).plan;
    approve(plan, &findings, req, &mount_table(), &fs())
}

/// The rejection those paths produce, or a panic if they are approved.
pub fn rejection(paths: &[(&str, u64)], req: &WriteRequest) -> GuardRejection {
    match approve_paths(paths, req) {
        Err(rejection) => rejection,
        Ok(verdict) => panic!("expected a rejection, got {verdict:?}"),
    }
}

/// The exit code the CLI would return for a rejection.
pub fn exit_code(rejection: GuardRejection) -> ExitCode {
    ExitCode::from(&BrozaError::from(rejection))
}

/// The pending approval for one cache file, or a panic.
pub fn pending_or_panic(request: &WriteRequest) -> broza::safety::PendingApproval {
    match approve_paths(&[(CACHE, 10)], request) {
        Ok(Verdict::NeedsConfirmation(pending)) => pending,
        other => panic!("expected a pending approval, got {other:?}"),
    }
}

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
