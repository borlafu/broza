//! The two narrower approvals: writes inside the quarantine store, and the
//! `tmutil` deletion of snapshots.
//!
//! Neither goes through the confirmation policy: the command that calls them has
//! already asked (`broza quarantine expire`, `restore`, `purge`).

use std::path::{Path, PathBuf};

use super::item::resolve_volume;
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
    let checked_root = canonicalize_no_follow(store_root, fs)?;
    let root = checked_root.path.clone();
    let entry = resolve_volume(&checked_root, mounts)?;
    allows_action(entry.volume.role, Action::Quarantine).map_err(|rejection| rejection.with_path(&root))?;
    let approved = paths_inside_store
        .iter()
        .map(|path| check_inside_store(path, &root, mounts, fs))
        .collect::<Result<Vec<ApprovedItem>, GuardRejection>>()?;
    Ok(issue::<QuarantineWrite>(approved))
}

/// One entry of the store: inside it, on the volume the mount table expects.
fn check_inside_store(
    path: &Path,
    root: &Path,
    mounts: &MountTable,
    fs: &dyn FileOps,
) -> Result<ApprovedItem, GuardRejection> {
    let checked = canonicalize_no_follow(path, fs)?;
    if checked.path == root || !checked.path.starts_with(root) {
        return Err(GuardRejection::OutsideQuarantineStore {
            path: checked.path,
            store_root: root.to_path_buf(),
        });
    }
    resolve_volume(&checked, mounts)?;
    Ok(ApprovedItem::from_checked(&checked))
}

/// The snapshot deletions of an approved plan, as their own token.
///
/// Borrows the approval: a plan may mix files to quarantine and snapshots to
/// delete, and the executor spends this token on the latter while the mover
/// keeps the original for the former. Only items the guard approved as
/// snapshots are carried over. The plan travels along unchanged for the
/// record; the provider reads only `items()`.
pub fn snapshot_deletions(approved: &Approved<Write>) -> Approved<SnapshotDelete> {
    let items: Vec<ApprovedItem> =
        approved.items().iter().filter(|item| item.snapshot().is_some()).cloned().collect();
    issue::<SnapshotDelete>(ApprovedPlan::new(approved.plan().clone(), items))
}

#[cfg(test)]
mod tests {
    use super::approve_quarantine_write;
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
            snapshot: None,
        }
    }

    fn approved(items: Vec<CleanItem>) -> Approved<Write> {
        let evidence =
            items.iter().map(|item| evidence_for(&item.path.display().to_string(), 2, 3)).collect();
        let plan = CleanPlan::dry_run(session(), items).unwrap_or_else(|error| panic!("{error}"));
        issue::<Write>(ApprovedPlan::new(plan, evidence))
    }

    #[test]
    fn only_the_snapshot_items_of_an_approval_are_carried_into_the_narrowed_token() {
        let reference = crate::model::SnapshotRef {
            volume: "disk3s5".parse().unwrap_or_else(|error| panic!("{error}")),
            name: "com.apple.TimeMachine.2026-09-20-101530.local".to_owned(),
            uuid: "00000021-1111-4222-8333-000000000021".to_owned(),
        };
        let plan = CleanPlan::dry_run(session(), vec![item("/Users/dana/a", Action::Quarantine)])
            .unwrap_or_else(|error| panic!("{error}"));
        let items = vec![
            evidence_for("/Users/dana/a", 2, 3),
            super::ApprovedItem::for_snapshot(Path::new("/System/Volumes/Data"), 2, &reference),
        ];
        let approval = issue::<Write>(ApprovedPlan::new(plan, items));

        let narrowed = super::snapshot_deletions(&approval);

        assert_eq!(narrowed.items().len(), 1);
        assert_eq!(narrowed.items()[0].snapshot(), Some(&reference));
        assert_eq!(approval.items().len(), 2, "the original approval is untouched");
    }

    #[test]
    fn an_approval_without_snapshots_narrows_to_an_empty_token() {
        let approval = approved(vec![item("/Users/dana/a", Action::Quarantine)]);

        let narrowed = super::snapshot_deletions(&approval);

        assert!(narrowed.items().is_empty());
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
}
