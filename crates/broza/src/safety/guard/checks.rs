//! The plan-level safety checks of `docs/cli-spec.md` §3.4.
//!
//! Order (the refusals that are pure usage errors come first, so a wrong command
//! line is reported as such and never as a path problem):
//!
//! 0. the findings must be a set: a duplicated identifier makes every later
//!    lookup ambiguous;
//! 1. `--apply` present, else the plan stays a dry run;
//! 2. red selection, and `--yes` with `--purge`, are refused outright;
//! 3. an inform-only finding in the selection refuses the whole plan;
//! 4. per item, in [`super::item`]: the action, the finding it belongs to, the
//!    path, the volume, the role, the allowlist, the exclusions and the size;
//! 5. no `inform_only` item survived into the plan;
//! 6. `--max-size`;
//! 7. the confirmation policy.
//!
//! An item whose path no longer exists is not a refusal: it is marked `skipped`
//! with `not_found` and contributes nothing to the totals. When *every* item has
//! vanished the verdict is [`Verdict::Nothing`], which writes nothing and exits `0`.

use super::item::Outcome;
use super::item::check_item;
use super::rebuild::rebuild;
use super::verdict::{check_quarantine_root, dry_run, nothing};
use super::{ApprovedPlan, PendingApproval, Verdict, WriteRequest, seal};
use crate::clean::PlanOutcome;
use crate::model::{Action, CleanItem, CleanPlan, Finding, Risk};
use crate::ports::{ConfirmationRequest, FileOps};
use crate::safety::policy::{ConfirmationMode, confirmation_policy};
use crate::safety::rejection::{GuardRejection, PolicyError};
use crate::scan::MountTable;

/// Runs every check and, if they pass, produces the pending approval.
///
/// `findings` must contain every finding the plan refers to: the guard reads the
/// risk, the category, the intended action and the paths from them rather than
/// trusting the caller. The planner's whole [`PlanOutcome`] is taken, so the
/// inform-only findings it set aside cannot be dropped on the way here.
pub fn approve(
    outcome: &PlanOutcome,
    findings: &[Finding],
    req: &WriteRequest,
    mounts: &MountTable,
    fs: &dyn FileOps,
) -> Result<Verdict, GuardRejection> {
    reject_duplicate_findings(findings)?;
    let quarantine_root = check_quarantine_root(req, mounts, fs)?;
    let plan = &outcome.plan;
    if !req.apply {
        return dry_run(plan, req);
    }
    let input = req.policy_input(max_risk(plan, findings)?, is_irreversible(plan, req));
    let mode = confirmation_policy(input);
    if let ConfirmationMode::Rejected(reason) = mode {
        return Err(PolicyError::Rejected(reason).into());
    }
    if let Some(informed) = outcome.informed_only.first() {
        return Err(GuardRejection::InformOnlySelected(informed.clone()));
    }
    if plan.items().is_empty() {
        return nothing(plan, req);
    }
    let payload =
        rebuild(plan, &check_items(plan, findings, req, mounts, fs)?)?.with_quarantine_root(quarantine_root);
    check_no_inform_only(&payload.plan)?;
    if payload.items.is_empty() {
        return nothing(&payload.plan, req);
    }
    check_max_size(&payload, req)?;
    if let Ok(stopped) = PolicyError::try_from(mode) {
        return Err(stopped.into());
    }
    let request = confirmation_request(&payload, req, findings);
    Ok(Verdict::NeedsConfirmation(PendingApproval { payload, mode, request, seal: seal::Seal }))
}

/// Check 0: two findings with the same identifier make `finding_of` a coin toss.
fn reject_duplicate_findings(findings: &[Finding]) -> Result<(), GuardRejection> {
    let duplicate = findings
        .iter()
        .enumerate()
        .find(|(index, finding)| findings[..*index].iter().any(|earlier| earlier.id() == finding.id()));
    match duplicate {
        Some((_, finding)) => {
            Err(GuardRejection::Inconsistent(format!("finding `{}` is listed more than once", finding.id())))
        }
        None => Ok(()),
    }
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

/// `true` when the plan destroys something no restore can bring back.
///
/// `--purge` is the obvious case, but emptying the trash and deleting a snapshot
/// are irreversible by nature, with or without the flag.
fn is_irreversible(plan: &CleanPlan, req: &WriteRequest) -> bool {
    req.purge || plan.items().iter().any(|item| matches!(item.action, Action::Purge | Action::TmutilDelete))
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

/// Checks every item in plan order, remembering where each one came from.
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
        .enumerate()
        .filter(|(_, item)| item.action != Action::InformOnly)
        .map(|(index, item)| {
            let finding = finding_of(item, findings)?;
            check_item(index, item, finding, req, &roots, mounts, fs)
        })
        .collect()
}

/// Check 6: what the plan will remove must fit under `--max-size`.
fn check_max_size(payload: &ApprovedPlan, req: &WriteRequest) -> Result<(), GuardRejection> {
    let planned_bytes = payload.plan.planned_bytes();
    if let Some(max_bytes) = req.max_size
        && planned_bytes > max_bytes
    {
        return Err(GuardRejection::MaxSizeExceeded { planned_bytes, max_bytes });
    }
    Ok(())
}

/// Check 5: a single `inform_only` item rejects the whole plan.
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
        total_bytes: payload.plan.planned_bytes(),
        irreversible: is_irreversible(&payload.plan, req),
        preview: payload.items.iter().map(|item| item.path().display().to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::approve;
    use crate::clean::PlanOutcome;
    use crate::model::{
        Action, Category, CleanItem, CleanPlan, Finding, FindingPath, ItemStatus, SessionId, Volume,
        VolumeRole,
    };
    use crate::safety::guard::{Verdict, WriteRequest};
    use crate::safety::rejection::GuardRejection;
    use crate::scan::{MountEntry, MountTable};
    use crate::testing::FakeFileOps;
    use std::path::PathBuf;

    const STORE: &str = "/Users/dana/.local/share/broza/quarantine";
    const CACHE: &str = "/Users/dana/Library/Caches/a";

    fn session() -> SessionId {
        "cln_20260921103608_a1b2".parse().unwrap_or_else(|error| panic!("{error}"))
    }

    fn finding(id: &str, category: Category, paths: &[&str]) -> Finding {
        Finding::builder(id.parse().unwrap_or_else(|error| panic!("{error}")), category, "title")
            .paths(
                paths
                    .iter()
                    .map(|path| FindingPath { path: PathBuf::from(path), size_bytes: 1, last_used: None })
                    .collect(),
            )
            .reclaimable_bytes(paths.len() as u64)
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

    fn outcome(items: Vec<CleanItem>) -> PlanOutcome {
        PlanOutcome {
            plan: CleanPlan::dry_run(session(), items).unwrap_or_else(|error| panic!("{error}")),
            informed_only: Vec::new(),
        }
    }

    fn table(role: VolumeRole) -> MountTable {
        MountTable::new(vec![MountEntry {
            mount_point: PathBuf::from("/"),
            device: 1,
            volume: Volume {
                id: "disk3s5".parse().unwrap_or_else(|error| panic!("{error}")),
                name: "test".to_owned(),
                uuid: None,
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
    fn an_already_applied_plan_without_apply_is_a_bug_in_the_caller() {
        let applied = CleanPlan::dry_run(session(), Vec::new())
            .and_then(|plan| plan.into_applied(Some(PathBuf::from(STORE))))
            .unwrap_or_else(|error| panic!("{error}"));
        let outcome = PlanOutcome { plan: applied, informed_only: Vec::new() };
        let rejection = approve(
            &outcome,
            &[],
            &WriteRequest::new("/Users/dana"),
            &table(VolumeRole::Data),
            &FakeFileOps::new(),
        )
        .err()
        .unwrap_or_else(|| panic!("an applied plan without --apply must be refused"));
        assert!(matches!(rejection, GuardRejection::Inconsistent(_)), "{rejection}");
    }

    /// Two findings with the same id make every lookup a coin toss, so the guard
    /// refuses before it has looked at anything else.
    #[test]
    fn duplicate_finding_identifiers_are_refused_in_either_order() {
        let cache = finding("user-cache.app", Category::UserCache, &[CACHE]);
        let twin = finding("user-cache.app", Category::UserCache, &["/Users/dana/other"]);
        let other = finding("trash.volumes", Category::Trash, &["/Users/dana/.Trash/x"]);
        for findings in [
            vec![cache.clone(), twin.clone()],
            vec![twin.clone(), cache.clone()],
            vec![other.clone(), cache.clone(), twin.clone()],
        ] {
            let rejection = approve(
                &outcome(Vec::new()),
                &findings,
                &WriteRequest::new("/Users/dana"),
                &table(VolumeRole::Data),
                &FakeFileOps::new(),
            )
            .err()
            .unwrap_or_else(|| panic!("a duplicated finding id must be refused"));
            assert!(matches!(rejection, GuardRejection::Inconsistent(_)), "{rejection}");
        }
    }

    #[test]
    fn an_item_whose_finding_is_missing_is_refused() {
        let fs = FakeFileOps::new().with_root("/", 1).with_sized_file(CACHE, 1);
        let rejection = approve(
            &outcome(vec![item(CACHE, Action::Quarantine)]),
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
        let findings = [finding("user-cache.app", Category::UserCache, &[CACHE])];
        let rejection = approve(
            &outcome(vec![item(CACHE, Action::Quarantine)]),
            &findings,
            &applying(),
            &MountTable::default(),
            &fs,
        )
        .err()
        .unwrap_or_else(|| panic!("an unmounted path must be refused"));
        assert_eq!(rejection, GuardRejection::UnknownVolume(CACHE.into()));
    }

    /// A mount table that disagrees with `lstat` is stale; the guard refuses
    /// rather than reasoning about the wrong volume.
    #[test]
    fn a_device_the_mount_table_does_not_expect_is_refused() {
        let fs = FakeFileOps::new().with_root("/", 99).with_sized_file(CACHE, 1);
        let findings = [finding("user-cache.app", Category::UserCache, &[CACHE])];
        let rejection = approve(
            &outcome(vec![item(CACHE, Action::Quarantine)]),
            &findings,
            &applying(),
            &table(VolumeRole::Data),
            &fs,
        )
        .err()
        .unwrap_or_else(|| panic!("a device mismatch must be refused"));
        assert_eq!(rejection, GuardRejection::UnknownVolume(CACHE.into()));
    }

    #[test]
    fn an_empty_plan_is_nothing_to_do() {
        let verdict =
            approve(&outcome(Vec::new()), &[], &applying(), &table(VolumeRole::Data), &FakeFileOps::new());
        assert!(matches!(verdict, Ok(Verdict::Nothing(_))), "{verdict:?}");
    }
}
