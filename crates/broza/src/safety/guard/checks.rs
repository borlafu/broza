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

#[cfg(test)]
mod tests {
    use super::{approve, approve_quarantine_write, narrow_to_snapshot_delete};
    use crate::model::{Action, CleanItem, CleanPlan, ItemStatus, SessionId, Volume, VolumeRole};
    use crate::safety::guard::{Write, WriteRequest, issue};
    use crate::safety::rejection::GuardRejection;
    use crate::safety::test_fs::MemFs;
    use crate::scan::{MountEntry, MountTable};
    use std::path::{Path, PathBuf};

    const STORE: &str = "/Users/dana/.local/share/broza/quarantine";

    fn session() -> SessionId {
        "cln_20260921103608_a1b2".parse().unwrap_or_else(|error| panic!("{error}"))
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

    fn store_fs() -> MemFs {
        MemFs::new().dir(STORE).file(format!("{STORE}/cln_20260921103608_a1b2/items/1/a"), 5)
    }

    #[test]
    fn an_already_applied_plan_without_apply_is_a_bug_in_the_caller() {
        let applied = plan(Vec::new())
            .into_applied(Some(PathBuf::from(STORE)))
            .unwrap_or_else(|error| panic!("{error}"));
        let rejection =
            approve(applied, &WriteRequest::new("/Users/dana"), &table(VolumeRole::Data), &MemFs::new())
                .err()
                .unwrap_or_else(|| panic!("an applied plan without --apply must be refused"));
        assert!(matches!(rejection, GuardRejection::Inconsistent(_)), "{rejection}");
    }

    #[test]
    fn an_item_whose_finding_id_has_no_known_category_is_refused() {
        let unknown = CleanItem {
            finding_id: "made-up.detector".parse().unwrap_or_else(|error| panic!("{error}")),
            ..item("/Users/dana/Library/Caches/a", Action::Quarantine)
        };
        let fs = MemFs::new().file("/Users/dana/Library/Caches/a", 1);
        let request = WriteRequest { apply: true, tty: true, ..WriteRequest::new("/Users/dana") };
        let rejection = approve(plan(vec![unknown]), &request, &table(VolumeRole::Data), &fs)
            .err()
            .unwrap_or_else(|| panic!("an unknown category must be refused"));
        assert!(matches!(rejection, GuardRejection::Inconsistent(_)), "{rejection}");
    }

    #[test]
    fn a_quarantine_store_on_a_protected_volume_is_refused() {
        let rejection =
            approve_quarantine_write(&[], Path::new(STORE), &table(VolumeRole::System), &store_fs())
                .err()
                .unwrap_or_else(|| panic!("a protected volume must be refused"));
        assert_eq!(
            rejection,
            GuardRejection::ProtectedVolume { path: STORE.into(), role: VolumeRole::System }
        );
    }

    #[test]
    fn a_store_path_that_is_not_already_canonical_is_refused() {
        let sneaky = PathBuf::from(format!("{STORE}/cln_20260921103608_a1b2/../../../Documents"));
        let refused =
            approve_quarantine_write(&[sneaky], Path::new(STORE), &table(VolumeRole::Data), &store_fs());
        assert!(refused.is_err(), "a path climbing out of the store must be refused");
    }

    #[test]
    fn an_unmounted_store_is_refused() {
        let rejection = approve_quarantine_write(&[], Path::new(STORE), &MountTable::default(), &store_fs())
            .err()
            .unwrap_or_else(|| panic!("an unknown volume must be refused"));
        assert_eq!(rejection, GuardRejection::UnknownVolume(STORE.into()));
    }

    #[test]
    fn only_a_plan_made_of_snapshot_deletions_can_be_narrowed() {
        let snapshots = issue::<Write>(plan(vec![item("/Users/dana/snap", Action::TmutilDelete)]));
        let narrowed = narrow_to_snapshot_delete(&snapshots).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(narrowed.plan().items().len(), 1);

        let mixed = issue::<Write>(plan(vec![
            item("/Users/dana/snap", Action::TmutilDelete),
            item("/Users/dana/Library/Caches/a", Action::Quarantine),
        ]));
        let rejection = narrow_to_snapshot_delete(&mixed)
            .err()
            .unwrap_or_else(|| panic!("a mixed plan is not a snapshot deletion"));
        assert!(matches!(rejection, GuardRejection::Inconsistent(_)), "{rejection}");
    }
}
