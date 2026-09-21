//! The two narrower approvals: writes inside the quarantine store, and the
//! `tmutil` deletion of snapshots.
//!
//! Neither goes through the confirmation policy: the command that calls them has
//! already asked (`broza quarantine expire`, `restore`, `purge`).

use std::path::{Path, PathBuf};

use super::token::issue;
use super::{Approved, ApprovedItem, ApprovedPlan, QuarantineWrite, SnapshotDelete, Write};
use crate::model::Action;
use crate::ports::FileOps;
use crate::safety::path::canonicalize_no_follow;
use crate::safety::rejection::GuardRejection;
use crate::safety::roles::allows_action;
use crate::scan::MountTable;

/// Approves a write inside the quarantine store: restore, expire or purge.
///
/// `store_root` must be absolute, symlink-free and on a volume Broza may write to;
/// every path must be a validated strict descendant of it.
pub fn approve_quarantine_write(
    paths_inside_store: &[PathBuf],
    store_root: &Path,
    mounts: &MountTable,
    fs: &dyn FileOps,
) -> Result<Approved<QuarantineWrite>, GuardRejection> {
    let root = canonicalize_no_follow(store_root, fs)?.path;
    let entry = mounts.volume_for(&root).ok_or_else(|| GuardRejection::UnknownVolume(root.clone()))?;
    allows_action(entry.volume.role, Action::Quarantine).map_err(|rejection| rejection.with_path(&root))?;
    let approved = paths_inside_store
        .iter()
        .map(|path| check_inside_store(path, &root, fs))
        .collect::<Result<Vec<ApprovedItem>, GuardRejection>>()?;
    Ok(issue::<QuarantineWrite>(approved))
}

fn check_inside_store(path: &Path, root: &Path, fs: &dyn FileOps) -> Result<ApprovedItem, GuardRejection> {
    let checked = canonicalize_no_follow(path, fs)?;
    if checked.path != root && checked.path.starts_with(root) {
        return Ok(ApprovedItem::from_checked(&checked));
    }
    Err(GuardRejection::OutsideQuarantineStore { path: checked.path, store_root: root.to_path_buf() })
}

/// Narrows a plan approved for writing to a snapshot deletion.
///
/// Takes the token by value: the same approval cannot also be spent on a
/// filesystem write. Every item must carry [`Action::TmutilDelete`], because
/// snapshots are removed by `tmutil` and never by unlinking files.
pub fn narrow_to_snapshot_delete(
    approved: Approved<Write>,
) -> Result<Approved<SnapshotDelete>, GuardRejection> {
    if let Some(item) = approved.plan().items().iter().find(|item| item.action != Action::TmutilDelete) {
        return Err(GuardRejection::Inconsistent(format!(
            "`{}` is not a snapshot deletion",
            item.path.display()
        )));
    }
    let items = approved.items().to_vec();
    Ok(issue::<SnapshotDelete>(ApprovedPlan::new(approved.into_plan(), items)))
}

#[cfg(test)]
mod tests {
    use super::{approve_quarantine_write, narrow_to_snapshot_delete};
    use crate::model::{Action, CleanItem, CleanPlan, ItemStatus, SessionId, Volume, VolumeRole};
    use crate::safety::guard::token::evidence_for;
    use crate::safety::guard::token::issue;
    use crate::safety::guard::{Approved, ApprovedPlan, Write};
    use crate::safety::rejection::GuardRejection;
    use crate::scan::{MountEntry, MountTable};
    use crate::testing::FakeFileOps;
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

    fn approved(items: Vec<CleanItem>) -> Approved<Write> {
        let evidence =
            items.iter().map(|item| evidence_for(&item.path.display().to_string(), 2, 3)).collect();
        let plan = CleanPlan::dry_run(session(), items).unwrap_or_else(|error| panic!("{error}"));
        issue::<Write>(ApprovedPlan::new(plan, evidence))
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

    fn store_fs() -> FakeFileOps {
        FakeFileOps::new()
            .with_root("/", 1)
            .with_dir(STORE)
            .with_sized_file(format!("{STORE}/cln_20260921103608_a1b2/items/1/a"), 5)
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
    fn a_store_path_that_climbs_out_of_the_store_is_refused() {
        let sneaky = PathBuf::from(format!("{STORE}/cln_20260921103608_a1b2/../../../Documents"));
        let refused =
            approve_quarantine_write(&[sneaky], Path::new(STORE), &table(VolumeRole::Data), &store_fs());
        assert!(
            matches!(refused, Err(GuardRejection::RelativeComponent(_))),
            "a `..` inside the store is still a `..`: {refused:?}"
        );
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
        let snapshots = approved(vec![item("/Users/dana/snap", Action::TmutilDelete)]);
        let narrowed = narrow_to_snapshot_delete(snapshots).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(narrowed.plan().items().len(), 1);
        assert_eq!(narrowed.items().len(), 1);

        let mixed = approved(vec![
            item("/Users/dana/snap", Action::TmutilDelete),
            item("/Users/dana/Library/Caches/a", Action::Quarantine),
        ]);
        let rejection = narrow_to_snapshot_delete(mixed)
            .err()
            .unwrap_or_else(|| panic!("a mixed plan is not a snapshot deletion"));
        assert!(matches!(rejection, GuardRejection::Inconsistent(_)), "{rejection}");
    }
}
