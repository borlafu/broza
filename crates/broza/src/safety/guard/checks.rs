//! The seven safety checks of `docs/cli-spec.md` §3.4.
//!
//! Order (the two refusals that are pure usage errors come first, so a wrong
//! command line is reported as such and never as a path problem):
//!
//! 1. `--apply` present, else the plan stays a dry run;
//! 2. red selection, and `--yes` with `--purge`, are refused outright;
//! 3. an inform-only finding in the selection refuses the whole plan;
//! 4. per item: validate the path, `lstat` every component, resolve the volume,
//!    check the role, the allowlist and the exclusions, and check that the action
//!    is the one the finding plus `--purge` imply;
//! 5. `--max-size`;
//! 6. no `inform_only` item survived into the plan;
//! 7. the confirmation policy.
//!
//! An item whose path no longer exists is not a refusal: it is marked `skipped`
//! with `not_found` and the rest of the plan proceeds.

use super::{ApprovedItem, ApprovedPlan, PendingApproval, Verdict, WriteRequest, seal};
use crate::model::{Action, CleanItem, CleanPlan, CleanPlanRepr, Finding, ItemErrorCode, ItemStatus, Risk};
use crate::ports::{ConfirmationRequest, FileOps};
use crate::safety::path::canonicalize_no_follow;
use crate::safety::policy::{ConfirmationMode, confirmation_policy};
use crate::safety::rejection::{GuardRejection, PolicyError};
use crate::safety::roles::allows_action;
use crate::safety::roots::{AllowedRoots, RootContext, is_allowed_root, is_under_allowed_root};
use crate::scan::MountTable;

/// Runs every check and, if they pass, produces the pending approval.
///
/// `findings` must contain every finding the plan refers to: the guard reads the
/// risk, the category and the intended action from them rather than trusting the
/// caller or re-parsing identifiers.
pub fn approve(
    plan: CleanPlan,
    findings: &[Finding],
    req: &WriteRequest,
    mounts: &MountTable,
    fs: &dyn FileOps,
) -> Result<Verdict, GuardRejection> {
    if !req.apply {
        return dry_run(plan);
    }
    let mode = confirmation_policy(req.policy_input(max_risk(&plan, findings)?));
    if let ConfirmationMode::Rejected(reason) = mode {
        return Err(PolicyError::Rejected(reason).into());
    }
    if let Some(informed) = req.informed_only.first() {
        return Err(GuardRejection::InformOnlySelected(informed.clone()));
    }
    if plan.items().is_empty() {
        return Ok(Verdict::Nothing);
    }
    let outcomes = check_items(&plan, findings, req, mounts, fs)?;
    let payload = rebuild(&plan, &outcomes)?;
    check_max_size(&payload, req)?;
    check_no_inform_only(&payload.plan)?;
    if let Ok(stopped) = PolicyError::try_from(mode) {
        return Err(stopped.into());
    }
    let request = confirmation_request(&payload, req, findings);
    Ok(Verdict::NeedsConfirmation(PendingApproval { payload, mode, request, seal: seal::Seal }))
}

/// Check 1: without `--apply` the plan must already be, and stay, a dry run.
fn dry_run(plan: CleanPlan) -> Result<Verdict, GuardRejection> {
    if !plan.is_dry_run() {
        return Err(GuardRejection::Inconsistent(format!(
            "clean plan `{}` is marked applied but `--apply` was not given",
            plan.session_id()
        )));
    }
    if plan.items().is_empty() {
        return Ok(Verdict::Nothing);
    }
    Ok(Verdict::DryRun(plan))
}

/// Highest risk among the findings the plan refers to; `None` for an empty plan.
fn max_risk(plan: &CleanPlan, findings: &[Finding]) -> Result<Option<Risk>, GuardRejection> {
    plan.items()
        .iter()
        .map(|item| finding_of(item, findings).map(Finding::risk))
        .collect::<Result<Vec<Risk>, GuardRejection>>()
        .map(std::vec::Vec::into_iter)
        .map(Iterator::max)
}

/// The finding an item came from; a plan that refers to an unknown one is a bug.
fn finding_of<'a>(item: &CleanItem, findings: &'a [Finding]) -> Result<&'a Finding, GuardRejection> {
    findings.iter().find(|finding| finding.id() == &item.finding_id).ok_or_else(|| {
        GuardRejection::Inconsistent(format!(
            "item `{}` refers to unknown finding `{}`",
            item.path.display(),
            item.finding_id
        ))
    })
}

/// What one item turned into during the checks.
enum Outcome {
    /// The path passed every check.
    Approved(ApprovedItem),
    /// The path is gone; the item is skipped and the plan proceeds.
    Missing,
}

/// Checks every item in plan order.
fn check_items(
    plan: &CleanPlan,
    findings: &[Finding],
    req: &WriteRequest,
    mounts: &MountTable,
    fs: &dyn FileOps,
) -> Result<Vec<Outcome>, GuardRejection> {
    let roots = req.allowed_roots()?;
    plan.items()
        .iter()
        .filter(|item| item.action != Action::InformOnly)
        .map(|item| check_item(item, findings, req, &roots, mounts, fs))
        .collect()
}

/// Checks 2 to 5 plus the `--purge` consistency rule for one item.
fn check_item(
    item: &CleanItem,
    findings: &[Finding],
    req: &WriteRequest,
    roots: &AllowedRoots,
    mounts: &MountTable,
    fs: &dyn FileOps,
) -> Result<Outcome, GuardRejection> {
    let finding = finding_of(item, findings)?;
    check_action_matches(item, finding, req)?;
    let checked = match canonicalize_no_follow(&item.path, fs) {
        Ok(checked) => checked,
        Err(rejection) if rejection.is_missing_path() => return Ok(Outcome::Missing),
        Err(rejection) => return Err(rejection),
    };
    let path = checked.path.as_path();
    let mount = mounts.volume_for(path).ok_or_else(|| GuardRejection::UnknownVolume(path.to_path_buf()))?;
    allows_action(mount.volume.role, item.action).map_err(|rejection| rejection.with_path(path))?;
    let context = RootContext { category: finding.category(), mount };
    if !is_under_allowed_root(path, roots, context) {
        return Err(if is_allowed_root(path, roots, context) {
            GuardRejection::RootItself(path.to_path_buf())
        } else {
            GuardRejection::OutsideAllowedRoots(path.to_path_buf())
        });
    }
    if req.exclusions.matches(path) {
        return Err(GuardRejection::Excluded(path.to_path_buf()));
    }
    Ok(Outcome::Approved(ApprovedItem::from_checked(&checked)))
}

/// The action must be exactly what the finding plus `--purge` imply.
///
/// `--purge` upgrades a quarantine to an irreversible deletion and nothing else;
/// a category whose own action is already `purge` (the trash) is unaffected.
fn check_action_matches(
    item: &CleanItem,
    finding: &Finding,
    req: &WriteRequest,
) -> Result<(), GuardRejection> {
    let expected = expected_action(finding.action(), req.purge);
    if item.action == expected {
        return Ok(());
    }
    Err(GuardRejection::Inconsistent(format!(
        "item `{}` has action `{:?}` but finding `{}` with purge={} implies `{expected:?}`",
        item.path.display(),
        item.action,
        finding.id(),
        req.purge,
    )))
}

/// `--purge` turns a quarantine into an irreversible deletion; nothing else changes.
pub(crate) fn expected_action(action: Action, purge: bool) -> Action {
    if purge && action == Action::Quarantine { Action::Purge } else { action }
}

/// Rebuilds the plan from the checked paths, marking the vanished ones `skipped`.
///
/// The result is no longer a dry run: it is the plan that will be applied, and it
/// carries only paths the guard has validated.
fn rebuild(plan: &CleanPlan, outcomes: &[Outcome]) -> Result<ApprovedPlan, GuardRejection> {
    let mut checked = outcomes.iter();
    let items = plan
        .items()
        .iter()
        .map(|item| match item.action {
            Action::InformOnly => item.clone(),
            _ => checked.next().map_or_else(|| item.clone(), |outcome| rebuilt_item(item, outcome)),
        })
        .collect();
    let repr = CleanPlanRepr {
        dry_run: false,
        session_id: plan.session_id().clone(),
        planned_bytes: plan.planned_bytes(),
        quarantined_bytes: 0,
        reclaimed_bytes: 0,
        quarantine_path: None,
        expired_sessions: Vec::new(),
        items,
    };
    let plan = CleanPlan::new(repr)
        .map_err(|error| GuardRejection::Inconsistent(format!("approved plan: {error}")))?;
    let items = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            Outcome::Approved(item) => Some(item.clone()),
            Outcome::Missing => None,
        })
        .collect();
    Ok(ApprovedPlan { plan, items })
}

fn rebuilt_item(item: &CleanItem, outcome: &Outcome) -> CleanItem {
    match outcome {
        Outcome::Approved(approved) => CleanItem { path: approved.path.clone(), ..item.clone() },
        Outcome::Missing => {
            CleanItem { status: ItemStatus::Skipped, error: Some(ItemErrorCode::NotFound), ..item.clone() }
        }
    }
}

/// Check 6: what the plan would actually remove must fit under `--max-size`.
///
/// Skipped items are not counted: they will not be removed.
fn check_max_size(payload: &ApprovedPlan, req: &WriteRequest) -> Result<(), GuardRejection> {
    let planned_bytes = removable_bytes(&payload.plan);
    if let Some(max_bytes) = req.max_size
        && planned_bytes > max_bytes
    {
        return Err(GuardRejection::MaxSizeExceeded { planned_bytes, max_bytes });
    }
    Ok(())
}

fn removable_bytes(plan: &CleanPlan) -> u64 {
    plan.items()
        .iter()
        .filter(|item| item.status == ItemStatus::Planned)
        .fold(0_u64, |sum, item| sum.saturating_add(item.size_bytes))
}

/// Check 7: a single `inform_only` item rejects the whole plan.
fn check_no_inform_only(plan: &CleanPlan) -> Result<(), GuardRejection> {
    match plan.items().iter().find(|item| item.action == Action::InformOnly) {
        Some(item) => Err(GuardRejection::InformOnlyItem(item.path.clone())),
        None => Ok(()),
    }
}

/// What the prompter shows the user.
fn confirmation_request(
    payload: &ApprovedPlan,
    req: &WriteRequest,
    findings: &[Finding],
) -> ConfirmationRequest {
    let max_risk = payload
        .plan
        .items()
        .iter()
        .filter_map(|item| finding_of(item, findings).ok())
        .map(Finding::risk)
        .max();
    ConfirmationRequest {
        max_risk: max_risk.unwrap_or(Risk::Green),
        item_count: payload.items.len(),
        total_bytes: removable_bytes(&payload.plan),
        irreversible: req.purge,
        preview: payload.items.iter().map(|item| item.path.display().to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::{approve, expected_action};
    use crate::model::{
        Action, Category, CleanItem, CleanPlan, Finding, ItemStatus, SessionId, Volume, VolumeRole,
    };
    use crate::safety::guard::WriteRequest;
    use crate::safety::rejection::GuardRejection;
    use crate::scan::{MountEntry, MountTable};
    use crate::testing::FakeFileOps;
    use std::path::PathBuf;

    const STORE: &str = "/Users/dana/.local/share/broza/quarantine";
    const CACHE: &str = "/Users/dana/Library/Caches/a";

    fn session() -> SessionId {
        "cln_20260921103608_a1b2".parse().unwrap_or_else(|error| panic!("{error}"))
    }

    fn finding(id: &str, category: Category) -> Finding {
        Finding::builder(id.parse().unwrap_or_else(|error| panic!("{error}")), category, "title")
            .build()
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn item(path: &str, action: Action) -> CleanItem {
        CleanItem {
            path: PathBuf::from(path),
            finding_id: "user-cache.app".parse().unwrap_or_else(|error| panic!("{error}")),
            size_bytes: 1,
            status: ItemStatus::Planned,
            action,
            error: None,
        }
    }

    fn plan(items: Vec<CleanItem>) -> CleanPlan {
        CleanPlan::dry_run(session(), items).unwrap_or_else(|error| panic!("{error}"))
    }

    fn table(role: VolumeRole) -> MountTable {
        MountTable::new(vec![MountEntry {
            mount_point: PathBuf::from("/"),
            device: 1,
            volume: Volume {
                id: "disk3s5".parse().unwrap_or_else(|error| panic!("{error}")),
                name: "test".to_owned(),
                role,
                mount_point: Some(PathBuf::from("/")),
                used_bytes: 0,
                writable_by_broza: role.writable_by_broza(),
                purpose: String::new(),
            },
            firmlinks: Vec::new(),
        }])
    }

    fn applying() -> WriteRequest {
        WriteRequest { apply: true, tty: true, ..WriteRequest::new("/Users/dana") }
    }

    #[test]
    fn purge_only_upgrades_a_quarantine() {
        assert_eq!(expected_action(Action::Quarantine, true), Action::Purge);
        assert_eq!(expected_action(Action::Quarantine, false), Action::Quarantine);
        assert_eq!(expected_action(Action::Purge, false), Action::Purge);
        assert_eq!(expected_action(Action::TmutilDelete, true), Action::TmutilDelete);
    }

    #[test]
    fn an_already_applied_plan_without_apply_is_a_bug_in_the_caller() {
        let applied = plan(Vec::new())
            .into_applied(Some(PathBuf::from(STORE)))
            .unwrap_or_else(|error| panic!("{error}"));
        let rejection = approve(
            applied,
            &[],
            &WriteRequest::new("/Users/dana"),
            &table(VolumeRole::Data),
            &FakeFileOps::new(),
        )
        .err()
        .unwrap_or_else(|| panic!("an applied plan without --apply must be refused"));
        assert!(matches!(rejection, GuardRejection::Inconsistent(_)), "{rejection}");
    }

    #[test]
    fn an_item_whose_finding_is_missing_is_refused() {
        let fs = FakeFileOps::new().with_root("/", 1).with_sized_file(CACHE, 1);
        let rejection = approve(
            plan(vec![item(CACHE, Action::Quarantine)]),
            &[],
            &applying(),
            &table(VolumeRole::Data),
            &fs,
        )
        .err()
        .unwrap_or_else(|| panic!("a plan without its findings must be refused"));
        assert!(matches!(rejection, GuardRejection::Inconsistent(_)), "{rejection}");
    }

    #[test]
    fn a_path_on_no_known_volume_is_refused() {
        let fs = FakeFileOps::new().with_root("/", 1).with_sized_file(CACHE, 1);
        let findings = [finding("user-cache.app", Category::UserCache)];
        let rejection = approve(
            plan(vec![item(CACHE, Action::Quarantine)]),
            &findings,
            &applying(),
            &MountTable::default(),
            &fs,
        )
        .err()
        .unwrap_or_else(|| panic!("an unmounted path must be refused"));
        assert_eq!(rejection, GuardRejection::UnknownVolume(CACHE.into()));
    }
}
