//! Executing an approved plan: the reversible part goes through the quarantine
//! mover, the irreversible part is done here (`docs/cli-spec.md` §3.4, §4.4).
//!
//! Every item is one of three actions. `quarantine` items are moved into a
//! session by [`quarantine_items`]; `purge` items — the trash, or anything the
//! user upgraded with `--purge` — are re-checked against the token's
//! `(device, inode)` immediately before being removed for good, and their
//! measured bytes go to `reclaimed_bytes`; `tmutil_delete` items are snapshot
//! deletions and are not executed yet (M4 step 3). A plan without `quarantine`
//! items creates no session at all, so the report carries no `quarantine_path`.
//!
//! Nothing here can write without the [`Approved<Write>`] the guard issued:
//! the items it removes are the token's items, checked again right before.

use crate::BrozaError;
use crate::model::{Action, CleanPlan, ItemErrorCode, ItemStatus, QuarantineSession, Warning};
use crate::ports::{Clock, FileOps};
use crate::quarantine::codes::changed_since_check;
use crate::quarantine::guarded::io_code;
use crate::quarantine::measure_dir_bytes;
use crate::quarantine::mover::{movable_items, plan_indices};
use crate::quarantine::{MoveRequest, quarantine_items};
use crate::safety::guard::{Approved, ApprovedItem, Write};

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
/// manifest), [`BrozaError::Other`] when the token and the plan disagree, and
/// [`BrozaError::Other`] for a `tmutil_delete` item until snapshot deletion
/// lands. A single item's failure is recorded in the plan and never aborts.
pub fn execute(
    token: &Approved<Write>,
    request: &MoveRequest,
    fs: &dyn FileOps,
    clock: &dyn Clock,
) -> Result<Executed, BrozaError> {
    reject_snapshot_deletions(token.plan())?;
    let (session, plan, warnings) = if movable_items(token.plan(), token.items())?.is_empty() {
        (None, token.plan().clone().into_applied(None)?, Vec::new())
    } else {
        let moved = quarantine_items(token, request, fs, clock)?;
        (Some(moved.session), moved.plan, moved.warnings)
    };
    let plan = purge_items(token, plan, fs)?;
    Ok(Executed { session, plan, warnings })
}

/// Snapshot deletion is M4 step 3; until then a plan that asks for it is refused
/// before anything else is touched.
fn reject_snapshot_deletions(plan: &CleanPlan) -> Result<(), BrozaError> {
    match plan.items().iter().find(|item| item.action == Action::TmutilDelete) {
        Some(item) => Err(BrozaError::Other(format!(
            "snapshot deletion (`{}`) is not implemented yet",
            item.path.display()
        ))),
        None => Ok(()),
    }
}

/// Remove every `purge` item for good, recording each outcome in the plan.
fn purge_items(token: &Approved<Write>, plan: CleanPlan, fs: &dyn FileOps) -> Result<CleanPlan, BrozaError> {
    let indices = plan_indices(token.plan(), token.items())?;
    let mut plan = plan;
    let mut freed = 0_u64;
    for (index, item) in indices.into_iter().zip(token.items()) {
        let is_purge = plan.items().get(index).is_some_and(|planned| planned.action == Action::Purge);
        if !is_purge {
            continue;
        }
        let (status, error, bytes) = purge_one(item, fs);
        freed = freed.saturating_add(bytes);
        plan = plan.with_item_status(index, status, error)?;
    }
    let (quarantined, reclaimed) = (plan.quarantined_bytes(), plan.reclaimed_bytes().saturating_add(freed));
    plan.with_bytes(quarantined, reclaimed)
}

/// Re-check one item and remove it; the bytes it held come from a measurement
/// taken right before, never from the plan.
fn purge_one(item: &ApprovedItem, fs: &dyn FileOps) -> (ItemStatus, Option<ItemErrorCode>, u64) {
    let current = match fs.metadata(item.path()) {
        Ok(current) => current,
        Err(error) => return (ItemStatus::Failed, Some(io_code(&error)), 0),
    };
    if current.device != item.device() || current.inode != item.inode() {
        return (ItemStatus::Failed, Some(changed_since_check()), 0);
    }
    let bytes = if current.is_dir {
        match measure_dir_bytes(fs, item.path(), None) {
            Ok(bytes) => bytes,
            Err(error) => return (ItemStatus::Failed, Some(io_code(&error)), 0),
        }
    } else {
        current.allocated_bytes
    };
    match fs.remove_tree(item.path()) {
        Ok(()) => (ItemStatus::Purged, None, bytes),
        Err(error) => (ItemStatus::Failed, Some(io_code(&error)), 0),
    }
}
