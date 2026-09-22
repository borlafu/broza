//! Checks 2 to 5 of `docs/cli-spec.md` §3.4, for one item of the plan.
//!
//! Everything an item claims is verified against something the guard can see for
//! itself: the finding it says it came from, the `lstat` of the path, and the
//! volume the mount table resolves it to. A plan is data from another layer, and
//! this module treats it as such.

use std::path::Path;

use super::{ApprovedItem, WriteRequest};
use crate::model::{Action, Category, CleanItem, Finding};
use crate::ports::FileOps;
use crate::safety::firmlink::{firmlink_spellings, is_volume_root};
use crate::safety::path::{CanonicalPath, canonicalize_no_follow};
use crate::safety::rejection::GuardRejection;
use crate::safety::roles::allows_action;
use crate::safety::roots::{AllowedRoots, RootContext, is_allowed_root, is_under_allowed_root};
use crate::scan::{MountEntry, MountTable};

/// What one item of the plan turned into during the checks.
pub(super) struct Outcome {
    /// Position of the item in the plan, so the rebuild cannot drift.
    pub(super) index: usize,
    /// What the checks concluded.
    pub(super) kind: OutcomeKind,
}

/// The two ways an item can survive the checks.
pub(super) enum OutcomeKind {
    /// The path passed every check.
    Approved(ApprovedItem),
    /// The path is gone; the item is skipped and the plan proceeds.
    Missing,
}

/// Runs every per-item check, in the order of the specification.
pub(super) fn check_item(
    index: usize,
    item: &CleanItem,
    finding: &Finding,
    req: &WriteRequest,
    roots: &AllowedRoots,
    mounts: &MountTable,
    fs: &dyn FileOps,
) -> Result<Outcome, GuardRejection> {
    check_action_matches(item, finding, req)?;
    if item.action == Action::TmutilDelete {
        return check_snapshot_item(index, item, finding, mounts);
    }
    check_path_belongs_to_finding(item, finding, mounts)?;
    let checked = match canonicalize_no_follow(&item.path, fs) {
        Ok(checked) => checked,
        Err(rejection) if rejection.is_missing_path() => {
            return Ok(Outcome { index, kind: OutcomeKind::Missing });
        }
        Err(rejection) => return Err(rejection),
    };
    let path = checked.path.as_path();
    check_outside_quarantine_store(path, req)?;
    let mount = resolve_volume(&checked, mounts)?;
    allows_action(mount.volume.role, item.action).map_err(|rejection| rejection.with_path(path))?;
    check_under_an_allowed_root(path, finding, roots, mount)?;
    if req.exclusions.matches(path, is_data_volume(mount)) {
        return Err(GuardRejection::Excluded(path.to_path_buf()));
    }
    check_size_not_understated(item, &checked)?;
    Ok(Outcome { index, kind: OutcomeKind::Approved(ApprovedItem::from_checked(&checked)) })
}

/// A snapshot deletion touches no path: the item names a snapshot the finding
/// listed as purgeable Time Machine work, on a volume the mount table knows,
/// mounted where the item says, with a role that allows `tmutil_delete`.
fn check_snapshot_item(
    index: usize,
    item: &CleanItem,
    finding: &Finding,
    mounts: &MountTable,
) -> Result<Outcome, GuardRejection> {
    let inconsistent = |reason: String| GuardRejection::Inconsistent(reason);
    let Some(reference) = &item.snapshot else {
        return Err(inconsistent(format!("`{}` deletes a snapshot but names none", item.path.display())));
    };
    if finding.category() != Category::Snapshots {
        return Err(inconsistent(format!("finding `{}` is not a snapshots finding", finding.id())));
    }
    let listed = finding.snapshots().iter().find(|snapshot| snapshot.name == reference.name);
    match listed {
        Some(snapshot) if snapshot.is_actionable() && snapshot.volume.as_ref() == Some(&reference.volume) => {
        }
        _ => {
            return Err(inconsistent(format!(
                "snapshot `{}` is not one the finding lists as purgeable on `{}`",
                reference.name, reference.volume
            )));
        }
    }
    let mount = mounts
        .entries()
        .iter()
        .find(|entry| entry.volume.id == reference.volume && entry.mount_point == item.path)
        .ok_or_else(|| GuardRejection::UnknownVolume(item.path.clone()))?;
    allows_action(mount.volume.role, Action::TmutilDelete)
        .map_err(|rejection| rejection.with_path(&item.path))?;
    let approved = ApprovedItem::for_snapshot(&item.path, mount.device, &reference.name);
    Ok(Outcome { index, kind: OutcomeKind::Approved(approved) })
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

/// The path must be one the finding actually reported, or inside one.
///
/// Without this, an item could borrow the identity of an unrelated finding and
/// inherit its category — `/Applications/Safari.app` attached to an `unused-apps`
/// finding that never mentioned Safari.
///
/// A reported path that is a volume root (`/`, a mount point, a firmlink prefix)
/// is ignored rather than honoured: as a prefix it would match the whole machine,
/// and no detector has any business naming one.
fn check_path_belongs_to_finding(
    item: &CleanItem,
    finding: &Finding,
    mounts: &MountTable,
) -> Result<(), GuardRejection> {
    let claimed = firmlink_spellings(&item.path);
    let belongs =
        finding.paths().iter().filter(|reported| !is_volume_root(&reported.path, mounts)).any(|reported| {
            firmlink_spellings(&reported.path)
                .iter()
                .any(|spelling| claimed.iter().any(|claim| claim.starts_with(spelling)))
        });
    if belongs {
        return Ok(());
    }
    Err(GuardRejection::Inconsistent(format!(
        "item `{}` is not one of the paths finding `{}` reported",
        item.path.display(),
        finding.id(),
    )))
}

/// The quarantine store holds what a previous run moved aside; a clean plan that
/// reaches into it would delete the user's safety net.
fn check_outside_quarantine_store(path: &Path, req: &WriteRequest) -> Result<(), GuardRejection> {
    let Some(store_root) = req.quarantine_root.as_ref() else {
        return Ok(());
    };
    let inside = firmlink_spellings(path)
        .iter()
        .any(|spelling| firmlink_spellings(store_root).iter().any(|root| spelling.starts_with(root)));
    if inside {
        return Err(GuardRejection::InsideQuarantineStore {
            path: path.to_path_buf(),
            store_root: store_root.clone(),
        });
    }
    Ok(())
}

/// The volume the path resolves to, cross-checked against the device `lstat` saw.
///
/// A mount table built a moment earlier can be stale; if it disagrees with the
/// filesystem the guard refuses rather than reasoning about the wrong volume.
pub(super) fn resolve_volume<'a>(
    checked: &CanonicalPath,
    mounts: &'a MountTable,
) -> Result<&'a MountEntry, GuardRejection> {
    let path = checked.path.as_path();
    let mount = mounts.volume_for(path).ok_or_else(|| GuardRejection::UnknownVolume(path.to_path_buf()))?;
    if mount.device != checked.metadata.device {
        return Err(GuardRejection::UnknownVolume(path.to_path_buf()));
    }
    Ok(mount)
}

fn check_under_an_allowed_root(
    path: &Path,
    finding: &Finding,
    roots: &AllowedRoots,
    mount: &MountEntry,
) -> Result<(), GuardRejection> {
    let context = RootContext { category: finding.category(), mount };
    if is_under_allowed_root(path, roots, context) {
        return Ok(());
    }
    Err(if is_allowed_root(path, roots, context) {
        GuardRejection::RootItself(path.to_path_buf())
    } else {
        GuardRejection::OutsideAllowedRoots(path.to_path_buf())
    })
}

/// A plan may not claim less than the file on disk actually holds.
///
/// `--max-size` and the summary are computed from the observed size, so a plan
/// that under-reports would slip past the cap. Directories keep the size the
/// scanner aggregated for their whole subtree: `lstat` only sees the directory
/// entry itself.
fn check_size_not_understated(item: &CleanItem, checked: &CanonicalPath) -> Result<(), GuardRejection> {
    let observed = checked.metadata.size_bytes;
    if checked.metadata.is_dir || item.size_bytes >= observed {
        return Ok(());
    }
    Err(GuardRejection::Inconsistent(format!(
        "item `{}` claims {} bytes but holds {observed}",
        item.path.display(),
        item.size_bytes,
    )))
}

fn is_data_volume(mount: &MountEntry) -> bool {
    mount.volume.role == crate::model::VolumeRole::Data
}
