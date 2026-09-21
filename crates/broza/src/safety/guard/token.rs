//! The `Approved` capability token and what it carries.
//!
//! Part of the `guard` module tree, which is the only code that can name the
//! private seal and therefore the only code that can build a token.

use std::fmt;
use std::marker::PhantomData;
use std::path::PathBuf;

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
/// Deletion of APFS local snapshots through `tmutil`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotDelete;

impl seal::Sealed for Write {}
impl seal::Sealed for QuarantineWrite {}
impl seal::Sealed for SnapshotDelete {}

/// One path the guard checked, with the identity it had at that moment.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedItem {
    /// The checked path: absolute, free of `.`/`..`, no symlinked component.
    pub path: PathBuf,
    /// `st_dev` observed during the check.
    pub device: u64,
    /// `st_ino` observed during the check.
    pub inode: u64,
}

impl ApprovedItem {
    /// Records the identity a path had when the guard checked it.
    pub(super) fn from_checked(checked: &CanonicalPath) -> Self {
        Self { path: checked.path.clone(), device: checked.metadata.device, inode: checked.metadata.inode }
    }
}

/// A plan the guard approved, with the per-path evidence behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedPlan {
    pub(super) plan: CleanPlan,
    pub(super) items: Vec<ApprovedItem>,
}

impl ApprovedPlan {
    /// Pairs a checked plan with the evidence for each of its writable paths.
    pub(super) fn new(plan: CleanPlan, items: Vec<ApprovedItem>) -> Self {
        Self { plan, items }
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
