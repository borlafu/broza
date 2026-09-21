//! The two verdicts that write nothing, and the check that the quarantine store
//! is a place Broza may write at all.

use super::item::resolve_volume;
use super::{Verdict, WriteRequest};
use crate::model::{CleanItem, CleanPlan, CleanPlanRepr};
use crate::ports::FileOps;
use crate::safety::path::canonicalize_no_follow;
use crate::safety::rejection::GuardRejection;
use crate::safety::roots::validate_quarantine_root;
use crate::scan::MountTable;

/// Check 0b: a quarantine store outside the allowlist would be a second, unchecked
/// write target.
///
/// Returns the store as the guard resolved it, so the token can carry the only
/// spelling of it that was actually checked.
pub(super) fn check_quarantine_root(
    req: &WriteRequest,
    mounts: &MountTable,
    fs: &dyn FileOps,
) -> Result<Option<std::path::PathBuf>, GuardRejection> {
    let Some(store_root) = req.quarantine_root.as_ref() else {
        return Ok(None);
    };
    let roots = req.allowed_roots()?;
    let checked = canonicalize_no_follow(store_root, fs).map_err(|error| GuardRejection::InvalidRoot {
        path: store_root.clone(),
        reason: error.to_string(),
    })?;
    let mount = resolve_volume(&checked, mounts)?;
    validate_quarantine_root(&checked.path, &roots, mount, mounts)?;
    Ok(Some(checked.path))
}

/// Check 1: without `--apply` the plan must already be, and stay, a dry run.
pub(super) fn dry_run(plan: &CleanPlan, req: &WriteRequest) -> Result<Verdict, GuardRejection> {
    if !plan.is_dry_run() {
        return Err(GuardRejection::Inconsistent(format!(
            "clean plan `{}` is marked applied but `--apply` was not given",
            plan.session_id()
        )));
    }
    if plan.items().is_empty() {
        return nothing(plan, req);
    }
    Ok(Verdict::DryRun(plan.clone()))
}

/// Nothing to write: the plan is rebuilt with every counter at zero, so a caller
/// cannot have the report echo bytes it invented.
pub(super) fn nothing(plan: &CleanPlan, req: &WriteRequest) -> Result<Verdict, GuardRejection> {
    let repr = CleanPlanRepr {
        dry_run: !req.apply,
        session_id: plan.session_id().clone(),
        planned_bytes: 0,
        quarantined_bytes: 0,
        reclaimed_bytes: 0,
        quarantine_path: None,
        expired_sessions: Vec::new(),
        items: plan.items().iter().map(|item| CleanItem { size_bytes: 0, ..item.clone() }).collect(),
    };
    CleanPlan::new(repr)
        .map(Verdict::Nothing)
        .map_err(|error| GuardRejection::Inconsistent(format!("empty plan: {error}")))
}
