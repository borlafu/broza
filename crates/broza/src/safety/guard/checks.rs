//! The seven safety checks of `docs/cli-spec.md` §3.4, in the order the
//! specification fixes them, plus the two narrower approvals.
//!
//! Every path is canonicalised without following symlinks, resolved to a volume
//! through the firmlink-aware mount table, checked against the protected roles and
//! the root allowlist, and matched against the exclusions. Only then are the total
//! size and the `inform_only` rule evaluated, and only then is the confirmation
//! policy consulted.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use super::{
    Approved, PendingApproval, QuarantineWrite, SnapshotDelete, Verdict, Write, WriteRequest, issue, seal,
};
use crate::model::{Action, Category, CleanItem, CleanPlan, Risk};
use crate::ports::{ConfirmationRequest, FileOps};
use crate::safety::path::{
    AllowedRoots, RootContext, canonicalize_no_follow, is_allowed_root, is_under_allowed_root,
};
use crate::safety::policy::confirmation_policy;
use crate::safety::rejection::{GuardRejection, PolicyError};
use crate::safety::roles::allows_action;
use crate::scan::MountTable;

/// Runs checks 1 to 7 and then the confirmation policy.
///
/// `inform_only` items are not path-checked: nothing is ever written for them and
/// check 7 rejects the whole plan anyway (exit `2`, `docs/cli-spec.md` §2).
pub fn approve(
    plan: CleanPlan,
    req: &WriteRequest,
    mounts: &MountTable,
    fs: &dyn FileOps,
) -> Result<Verdict, GuardRejection> {
    if !req.apply {
        return dry_run(plan);
    }
    let roots = req.allowed_roots();
    for item in plan.items().iter().filter(|item| item.action != Action::InformOnly) {
        check_item(item, req, &roots, mounts, fs)?;
    }
    check_max_size(&plan, req)?;
    check_no_inform_only(&plan)?;
    let mode = confirmation_policy(req.policy_input());
    if let Ok(stopped) = PolicyError::try_from(mode) {
        return Err(stopped.into());
    }
    let request = confirmation_request(&plan, req);
    Ok(Verdict::NeedsConfirmation(PendingApproval { plan, mode, request, seal: seal::Seal }))
}

/// Check 1: without `--apply` the plan must already be, and stay, a dry run.
fn dry_run(plan: CleanPlan) -> Result<Verdict, GuardRejection> {
    if plan.is_dry_run() {
        return Ok(Verdict::DryRun(plan));
    }
    Err(GuardRejection::Inconsistent(format!(
        "clean plan `{}` is marked applied but `--apply` was not given",
        plan.session_id()
    )))
}

/// Checks 2 to 5 for one item.
fn check_item(
    item: &CleanItem,
    req: &WriteRequest,
    roots: &AllowedRoots,
    mounts: &MountTable,
    fs: &dyn FileOps,
) -> Result<(), GuardRejection> {
    let canonical = canonicalize_no_follow(&item.path, fs)?;
    let entry =
        mounts.volume_for(&canonical).ok_or_else(|| GuardRejection::UnknownVolume(canonical.clone()))?;
    let role = entry.volume.role;
    allows_action(role, item.action).map_err(|rejection| rejection.with_path(&canonical))?;
    let context = RootContext { category: category_of(item)?, role };
    if !is_under_allowed_root(&canonical, roots, context) {
        return Err(if is_allowed_root(&canonical, roots) {
            GuardRejection::RootItself(canonical)
        } else {
            GuardRejection::OutsideAllowedRoots(canonical)
        });
    }
    if req.exclusions.matches(&canonical) {
        return Err(GuardRejection::Excluded(canonical));
    }
    Ok(())
}

/// The category an item belongs to, taken from the finding it came from.
fn category_of(item: &CleanItem) -> Result<Category, GuardRejection> {
    Category::from_str(item.finding_id.category_part())
        .map_err(|error| GuardRejection::Inconsistent(format!("item `{}`: {error}", item.path.display())))
}

/// Check 6: the plan must fit under `--max-size`.
fn check_max_size(plan: &CleanPlan, req: &WriteRequest) -> Result<(), GuardRejection> {
    let planned_bytes = plan.planned_bytes();
    if let Some(max_bytes) = req.max_size
        && planned_bytes > max_bytes
    {
        return Err(GuardRejection::MaxSizeExceeded { planned_bytes, max_bytes });
    }
    Ok(())
}

/// Check 7: a single `inform_only` item rejects the whole plan.
fn check_no_inform_only(plan: &CleanPlan) -> Result<(), GuardRejection> {
    match plan.items().iter().find(|item| item.action == Action::InformOnly) {
        Some(item) => Err(GuardRejection::InformOnlyItem(item.path.clone())),
        None => Ok(()),
    }
}

/// What the prompter shows the user.
fn confirmation_request(plan: &CleanPlan, req: &WriteRequest) -> ConfirmationRequest {
    ConfirmationRequest {
        max_risk: req.max_risk.unwrap_or(Risk::Green),
        item_count: plan.items().len(),
        total_bytes: plan.planned_bytes(),
        irreversible: req.purge,
        preview: plan.items().iter().map(|item| item.path.display().to_string()).collect(),
    }
}

/// Approves a write inside the quarantine store: restore, expire or purge.
///
/// `store_root` must be absolute, symlink-free and on a volume Broza may write to;
/// every path must be a canonical strict descendant of it.
pub fn approve_quarantine_write(
    paths_inside_store: &[PathBuf],
    store_root: &Path,
    mounts: &MountTable,
    fs: &dyn FileOps,
) -> Result<Approved<QuarantineWrite>, GuardRejection> {
    let root = canonicalize_no_follow(store_root, fs)?;
    let entry = mounts.volume_for(&root).ok_or_else(|| GuardRejection::UnknownVolume(root.clone()))?;
    allows_action(entry.volume.role, Action::Quarantine).map_err(|rejection| rejection.with_path(&root))?;
    let approved = paths_inside_store
        .iter()
        .map(|path| check_inside_store(path, &root, fs))
        .collect::<Result<Vec<PathBuf>, GuardRejection>>()?;
    Ok(issue::<QuarantineWrite>(approved))
}

fn check_inside_store(path: &Path, root: &Path, fs: &dyn FileOps) -> Result<PathBuf, GuardRejection> {
    let canonical = canonicalize_no_follow(path, fs)?;
    if canonical == path && canonical != root && canonical.starts_with(root) {
        return Ok(canonical);
    }
    Err(GuardRejection::OutsideQuarantineStore { path: canonical, store_root: root.to_path_buf() })
}

/// Narrows a plan approved for writing to a snapshot deletion.
///
/// Snapshots are removed by `tmutil`, never by unlinking files, so every item of
/// the plan must carry [`Action::TmutilDelete`].
pub fn narrow_to_snapshot_delete(
    approved: &Approved<Write>,
) -> Result<Approved<SnapshotDelete>, GuardRejection> {
    let plan = approved.plan();
    match plan.items().iter().find(|item| item.action != Action::TmutilDelete) {
        Some(item) => {
            Err(GuardRejection::Inconsistent(format!("`{}` is not a snapshot deletion", item.path.display())))
        }
        None => Ok(issue::<SnapshotDelete>(plan.clone())),
    }
}
