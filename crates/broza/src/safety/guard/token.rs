//! The `Approved` capability token and what it carries.
//!
//! Part of the `guard` module tree, which is the only code that can name the
//! private seal and therefore the only code that can build a token.

use std::fmt;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use crate::model::SnapshotRef;

use super::seal;
use crate::model::CleanPlan;
use crate::safety::path::CanonicalPath;

/// What an [`Approved`] token authorises.
pub trait WriteKind: seal::Sealed {
    /// What the token carries to the executor.
    type Payload;
    /// Short description, used in `Debug` output.
    const DESCRIPTION: &'static str;
}

/// Execution of a clean plan (quarantine or purge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Write;
/// A write inside the quarantine store (restore, expire, purge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuarantineWrite;
/// A write *out of* the quarantine store: where a restore puts an item back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreWrite;
/// Deletion of APFS local snapshots through `tmutil`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotDelete;

impl seal::Sealed for Write {}
impl seal::Sealed for QuarantineWrite {}
impl seal::Sealed for RestoreWrite {}
impl seal::Sealed for SnapshotDelete {}

/// One item the guard approved: a path with the identity it had at that
/// moment, or a snapshot on a volume.
///
/// # Time of check, time of use
///
/// The guard checks a path and the executor writes to it some milliseconds later;
/// in between, anything on a writable volume can be renamed or replaced. Nothing
/// in user space can close that window completely, so Broza narrows it instead:
/// the token records the `(device, inode)` of the `lstat` that approved the path,
/// and **every mutating function must `lstat` again and refuse when the pair no
/// longer matches**. That turns a silent "deleted the wrong thing" into a skipped
/// item. The re-check is the executor's obligation (M3); the guard only supplies
/// the evidence.
///
/// # Two kinds of item
///
/// A snapshot item names no path the executor may write: its `path` is the
/// volume's mount point, kept so the plan item can be found, and the provider
/// deletes that UUID on that volume and nothing else. The distinction is a
/// type: only [`ApprovedItem::writable`] yields a [`WritablePath`], and only a
/// `WritablePath` carries an inode to re-check or a size to report, so a
/// consumer of `items()` cannot mistake a snapshot for something to move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedItem {
    target: Target,
}

/// What an approved item points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A path the executor may move or remove, with its checked identity.
    Path(WritablePath),
    /// A snapshot the provider may delete; no path is written.
    Snapshot {
        /// Mount point of the snapshot's volume: the plan item's path.
        mount_point: PathBuf,
        /// `st_dev` of that volume.
        device: u64,
        /// The snapshot, by name, UUID and volume.
        snapshot: SnapshotRef,
    },
}

/// A path the guard checked, with the identity `lstat` gave it at that moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WritablePath {
    /// The checked path: absolute, free of `.`/`..`, no symlinked component.
    path: PathBuf,
    /// `st_dev` observed during the check.
    device: u64,
    /// `st_ino` observed during the check.
    inode: u64,
    /// `st_size` observed during the check.
    size_bytes: u64,
    /// Allocated bytes observed during the check: what the disk gives back.
    allocated_bytes: u64,
    /// `true` when the leaf is a directory.
    is_dir: bool,
}

impl WritablePath {
    /// The checked path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `st_dev` at the moment of the check; re-`lstat` and compare before writing.
    pub fn device(&self) -> u64 {
        self.device
    }

    /// `st_ino` at the moment of the check; re-`lstat` and compare before writing.
    pub fn inode(&self) -> u64 {
        self.inode
    }

    /// Size of the leaf itself, as `lstat` reported it during the check.
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// Allocated bytes of the leaf at check time: the figure every byte
    /// counter Broza prints is in (`docs/cli-spec.md` §4.4).
    pub fn allocated_bytes(&self) -> u64 {
        self.allocated_bytes
    }

    /// `true` when the leaf is a directory, whose size the scanner aggregated.
    pub fn is_dir(&self) -> bool {
        self.is_dir
    }

    /// `true` when the sizes of this item are the leaf's own, measured by the
    /// guard, rather than the scanner's aggregate of a subtree.
    ///
    /// Only files are measured: `lstat` on a directory reports the size of the
    /// directory entry, not of its contents, and the guard does not walk trees.
    /// A directory's size is the scanner's aggregate, which may be stale — the
    /// executor re-measures it immediately before removal and abandons the item
    /// when the cap would be exceeded (`docs/cli-spec.md` §3.4).
    pub fn size_verified(&self) -> bool {
        !self.is_dir
    }
}

impl ApprovedItem {
    /// Records the identity a path had when the guard checked it.
    pub(super) fn from_checked(checked: &CanonicalPath) -> Self {
        Self {
            target: Target::Path(WritablePath {
                path: checked.path.clone(),
                device: checked.metadata.device,
                inode: checked.metadata.inode,
                size_bytes: checked.metadata.size_bytes,
                allocated_bytes: checked.metadata.allocated_bytes,
                is_dir: checked.metadata.is_dir,
            }),
        }
    }

    /// Records a snapshot deletion the guard approved: the volume's mount point
    /// and device, and the snapshot itself. There is no inode to re-check; the
    /// provider deletes that UUID on that volume and nothing else.
    pub(super) fn for_snapshot(mount_point: &Path, device: u64, snapshot: &SnapshotRef) -> Self {
        Self {
            target: Target::Snapshot {
                mount_point: mount_point.to_path_buf(),
                device,
                snapshot: snapshot.clone(),
            },
        }
    }

    /// What this item points at.
    pub fn target(&self) -> &Target {
        &self.target
    }

    /// The path the executor may write, with its identity; `None` for a
    /// snapshot, which has no such path.
    pub fn writable(&self) -> Option<&WritablePath> {
        match &self.target {
            Target::Path(path) => Some(path),
            Target::Snapshot { .. } => None,
        }
    }

    /// The snapshot this item deletes, for `tmutil_delete` items.
    pub fn snapshot(&self) -> Option<&SnapshotRef> {
        match &self.target {
            Target::Snapshot { snapshot, .. } => Some(snapshot),
            Target::Path(_) => None,
        }
    }

    /// The plan item's path: the checked path, or a snapshot's mount point.
    /// For matching plan items and naming the item in messages; a write goes
    /// through [`Self::writable`].
    pub fn path(&self) -> &Path {
        match &self.target {
            Target::Path(path) => path.path(),
            Target::Snapshot { mount_point, .. } => mount_point,
        }
    }

    /// `st_dev` of the path, or of the snapshot's volume.
    pub fn device(&self) -> u64 {
        match &self.target {
            Target::Path(path) => path.device(),
            Target::Snapshot { device, .. } => *device,
        }
    }
}

/// A plan the guard approved, with the per-path evidence behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedPlan {
    pub(super) plan: CleanPlan,
    pub(super) items: Vec<ApprovedItem>,
    pub(super) quarantine_root: Option<PathBuf>,
}

impl ApprovedPlan {
    /// Pairs a checked plan with the evidence for each of its writable paths.
    pub(super) fn new(plan: CleanPlan, items: Vec<ApprovedItem>) -> Self {
        Self { plan, items, quarantine_root: None }
    }

    /// Records the quarantine store the guard validated for this run.
    pub(super) fn with_quarantine_root(self, quarantine_root: Option<PathBuf>) -> Self {
        Self { quarantine_root, ..self }
    }
}

impl WriteKind for Write {
    type Payload = ApprovedPlan;
    const DESCRIPTION: &'static str = "clean plan execution";
}

impl WriteKind for QuarantineWrite {
    type Payload = Vec<ApprovedItem>;
    const DESCRIPTION: &'static str = "quarantine store write";
}

impl WriteKind for RestoreWrite {
    type Payload = Vec<PathBuf>;
    const DESCRIPTION: &'static str = "restore destination";
}

impl WriteKind for SnapshotDelete {
    type Payload = ApprovedPlan;
    const DESCRIPTION: &'static str = "snapshot deletion";
}

/// Proof that the safety kernel approved a write. Cannot be constructed elsewhere.
///
/// Deliberately not `Clone`, `Default` or `Deserialize`: a token is a decision
/// that happened, not data that can be copied or revived. The compile-fail cases
/// in `crates/broza/tests/compile_fail/` keep it that way.
pub struct Approved<K: WriteKind> {
    payload: K::Payload,
    /// Never read: its type is the point. Only `guard` can name it, so only
    /// `guard` can build this struct (ADR 0003, "private unit-struct seal").
    #[allow(dead_code)]
    seal: seal::Seal,
    kind: PhantomData<fn() -> K>,
}

impl<K: WriteKind> fmt::Debug for Approved<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Approved<{}>", K::DESCRIPTION)
    }
}

impl<K: WriteKind<Payload = ApprovedPlan>> Approved<K> {
    /// The approved plan. Every item path is the one the guard checked.
    pub fn plan(&self) -> &CleanPlan {
        &self.payload.plan
    }

    /// The paths that may be written, with the identity to re-check first.
    pub fn items(&self) -> &[ApprovedItem] {
        &self.payload.items
    }

    /// Consumes the token and yields the plan.
    pub fn into_plan(self) -> CleanPlan {
        self.payload.plan
    }

    /// The quarantine store the guard validated for this run, if there is one.
    ///
    /// The mover takes the root from here rather than from its caller: the
    /// store is a write target like any other, and the only one that was
    /// actually checked (`docs/cli-spec.md` §3.4, check 0b) is this one.
    pub fn quarantine_root(&self) -> Option<&Path> {
        self.payload.quarantine_root.as_deref()
    }
}

impl Approved<RestoreWrite> {
    /// The destinations a restore may write to.
    pub fn targets(&self) -> &[PathBuf] {
        &self.payload
    }

    /// `true` when `path` is one of them.
    pub fn covers(&self, path: &Path) -> bool {
        self.payload.iter().any(|target| target == path)
    }
}

impl Approved<QuarantineWrite> {
    /// The approved entries, all inside the quarantine store.
    pub fn items(&self) -> &[ApprovedItem] {
        &self.payload
    }

    /// Consumes the token and yields the entries.
    pub fn into_items(self) -> Vec<ApprovedItem> {
        self.payload
    }
}

/// Builds a token. The only constructor of [`Approved`] in the whole crate.
pub(super) fn issue<K: WriteKind>(payload: K::Payload) -> Approved<K> {
    Approved { payload, seal: seal::Seal, kind: PhantomData }
}

/// Evidence for a path the guard did not really check, for tests inside `guard`.
#[cfg(test)]
pub(super) fn evidence_for(path: &str, device: u64, inode: u64) -> ApprovedItem {
    ApprovedItem::from_checked(&CanonicalPath {
        path: PathBuf::from(path),
        metadata: crate::ports::EntryMetadata {
            device,
            inode,
            size_bytes: 1,
            allocated_bytes: 1,
            link_count: 1,
            is_dir: false,
            is_symlink: false,
            is_dataless: false,
            modified: None,
            accessed: None,
        },
    })
}
