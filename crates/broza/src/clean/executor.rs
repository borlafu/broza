//! Executing an approved plan: the reversible part goes through the quarantine
//! mover, the irreversible part is done here (`docs/cli-spec.md` §3.4, §4.4).
//!
//! Every item is one of three actions. `quarantine` items are moved into a
//! session by [`quarantine_items`]; `purge` items — the trash, or anything the
//! user upgraded with `--purge` — are re-checked against the token's
//! `(device, inode)` immediately before being removed for good, and their
//! measured bytes go to `reclaimed_bytes`; `tmutil_delete` items are handed to
//! the [`SnapshotProvider`] with a token narrowed to exactly those snapshots
//! (`diskutil apfs deleteSnapshot <volume> -uuid <uuid>`, one snapshot on one
//! volume), and count as `purged` with no bytes (macOS reports no snapshot size). A plan
//! without `quarantine` items creates no session at all, so the report carries
//! no `quarantine_path`.
//!
//! Nothing here can write without the [`Approved<Write>`] the guard issued:
//! the items it removes are the token's items, checked again right before.

use crate::BrozaError;
use crate::model::{Action, CleanPlan, ItemErrorCode, ItemStatus, QuarantineSession, Warning};
use crate::ports::{Clock, FileOps, SnapshotProvider};
use crate::quarantine::codes::max_size_exceeded;
use crate::quarantine::guarded::{io_code, recheck_identity};
use crate::quarantine::mover::{movable_items, plan_indices};
use crate::quarantine::{MoveRequest, exceeds_cap, measure_freed_bytes, quarantine_items};
use crate::safety::guard::{Approved, ApprovedItem, Write, snapshot_deletions};

/// Warning code: `tmutil` refused a snapshot deletion for lack of privileges.
pub const SNAPSHOT_NEEDS_ADMIN_CODE: &str = "snapshot_needs_admin";

/// What executing a plan left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executed {
    /// The quarantine session, when at least one item was to be moved.
    pub session: Option<QuarantineSession>,
    /// The plan with every item's outcome and both byte counters filled in.
    pub plan: CleanPlan,
    /// What the user should know about how the items were handled.
    pub warnings: Vec<Warning>,
}

/// Execute every item of the approved plan.
///
/// # Errors
///
/// Whatever the mover reports as fatal (claiming the session, writing the
/// manifest) and [`BrozaError::Other`] when the token and the plan disagree.
/// A single item's failure is recorded in the plan and never aborts.
pub fn execute(
    token: &Approved<Write>,
    request: &MoveRequest,
    fs: &dyn FileOps,
    clock: &dyn Clock,
    snapshots: &dyn SnapshotProvider,
) -> Result<Executed, BrozaError> {
    let (session, plan, mut warnings) = if movable_items(token.plan(), token.items())?.is_empty() {
        (None, token.plan().clone().into_applied(None)?, Vec::new())
    } else {
        let moved = quarantine_items(token, request, fs, clock)?;
        (Some(moved.session), moved.plan, moved.warnings)
    };
    let plan = purge_items(token, plan, fs, request.max_size)?;
    let plan = delete_snapshots(token, plan, snapshots, &mut warnings)?;
    Ok(Executed { session, plan, warnings })
}

/// Delete every `tmutil_delete` item through the provider, with a token that
/// covers exactly those snapshots.
fn delete_snapshots(
    token: &Approved<Write>,
    plan: CleanPlan,
    snapshots: &dyn SnapshotProvider,
    warnings: &mut Vec<Warning>,
) -> Result<CleanPlan, BrozaError> {
    let indices = plan_indices(token.plan(), token.items())?;
    let narrowed = snapshot_deletions(token);
    let mut plan = plan;
    for (index, item) in indices.into_iter().zip(token.items()) {
        let is_snapshot =
            plan.items().get(index).is_some_and(|planned| planned.action == Action::TmutilDelete);
        if !is_snapshot {
            continue;
        }
        let (status, error) = match snapshots.delete(&narrowed, item) {
            Ok(()) => (ItemStatus::Purged, None),
            Err(BrozaError::PermissionDenied { .. }) => {
                warnings.push(needs_admin(item));
                (ItemStatus::Failed, Some(ItemErrorCode::PermissionDenied))
            }
            Err(error) => (ItemStatus::Failed, Some(io_code(&error))),
        };
        plan = plan.with_item_status(index, status, error)?;
    }
    Ok(plan)
}

/// The exact command the user may run themselves; Broza never invokes `sudo`
/// (`docs/cli-spec.md` §6).
fn needs_admin(item: &ApprovedItem) -> Warning {
    let message = match item.snapshot() {
        Some(snapshot) => format!(
            "deleting snapshot `{}` needs administrator privileges; run: sudo diskutil apfs deleteSnapshot {} -uuid {}",
            snapshot.name, snapshot.volume, snapshot.uuid
        ),
        None => "deleting a snapshot needs administrator privileges".to_owned(),
    };
    Warning { code: SNAPSHOT_NEEDS_ADMIN_CODE.to_owned(), message, path: Some(item.path().to_path_buf()) }
}

/// Remove every `purge` item for good, recording each outcome in the plan.
///
/// `--max-size` is one cap for the whole run: the running total starts at what
/// the mover already quarantined, and an item whose measured size would take it
/// past the cap is left in place as `skipped` (`docs/cli-spec.md` §3.4, check 6).
fn purge_items(
    token: &Approved<Write>,
    plan: CleanPlan,
    fs: &dyn FileOps,
    max_size: Option<u64>,
) -> Result<CleanPlan, BrozaError> {
    let indices = plan_indices(token.plan(), token.items())?;
    let mut plan = plan;
    let mut running = plan.quarantined_bytes();
    let mut freed = 0_u64;
    for (index, item) in indices.into_iter().zip(token.items()) {
        let is_purge = plan.items().get(index).is_some_and(|planned| planned.action == Action::Purge);
        if !is_purge {
            continue;
        }
        let (status, error, bytes) = purge_one(item, fs, running, max_size);
        running = running.saturating_add(bytes);
        freed = freed.saturating_add(bytes);
        plan = plan.with_item_status(index, status, error)?;
    }
    let (quarantined, reclaimed) = (plan.quarantined_bytes(), plan.reclaimed_bytes().saturating_add(freed));
    plan.with_bytes(quarantined, reclaimed)
}

/// Re-check one item, measure what removing it frees, check the cap, remove it.
///
/// The bytes come from a measurement taken right before, never from the plan,
/// and count only files with a single name: a hard-linked file frees nothing.
/// A removal that fails half-way is recorded as `failed` with `0` bytes, which
/// under-reports what was freed rather than guessing.
fn purge_one(
    item: &ApprovedItem,
    fs: &dyn FileOps,
    running: u64,
    max_size: Option<u64>,
) -> (ItemStatus, Option<ItemErrorCode>, u64) {
    let current = match recheck_identity(item, fs) {
        Ok(current) => current,
        Err(code) => return (ItemStatus::Failed, Some(code), 0),
    };
    let bytes = if current.is_dir {
        match measure_freed_bytes(fs, item.path()) {
            Ok(bytes) => bytes,
            Err(error) => return (ItemStatus::Failed, Some(io_code(&error)), 0),
        }
    } else if current.link_count > 1 {
        0
    } else {
        current.allocated_bytes
    };
    if exceeds_cap(running, bytes, max_size) {
        return (ItemStatus::Skipped, Some(max_size_exceeded()), 0);
    }
    match fs.remove_tree(item.path()) {
        Ok(()) => (ItemStatus::Purged, None, bytes),
        Err(error) => (ItemStatus::Failed, Some(io_code(&error)), 0),
    }
}
