//! Moving the items of an approved plan into the quarantine store.
//!
//! The order is the one ADR 0004 fixes, chosen so that a crash at any point
//! leaves a store a later run can still read:
//!
//! 1. create the session directory and write a manifest in state `in_progress`;
//! 2. for each item, in plan order: re-`lstat` and compare `(device, inode)`
//!    with the token, compare the device with the store's, measure a directory,
//!    `rename` ([`attempt`](crate::quarantine::attempt));
//! 3. rewrite the manifest after every item (temp file + rename);
//! 4. write the manifest one last time in state `complete`.
//!
//! Nothing is ever removed here. A failed item stays exactly where it was and is
//! recorded as `failed` or `skipped`, and the run continues
//! (`docs/cli-spec.md` §3.4).

use std::path::PathBuf;
use std::time::Duration;

use crate::BrozaError;
use crate::model::{CleanPlan, ItemStatus, QuarantineSession, SessionState};
use crate::ports::{Clock, FileOps};
use crate::quarantine::attempt::{Destination, attempt_move, moved_of, outcome_of, updated_entry};
use crate::quarantine::entries::{planned_entries, sequence_of, with_entry};
use crate::quarantine::manifest::{self, Manifest};
use crate::quarantine::{layout, ttl};
use crate::safety::guard::{Approved, ApprovedItem, Write};

/// Where a move puts its items, and under which limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRequest {
    /// Root of the quarantine store; the session directory is created inside it.
    pub root: PathBuf,
    /// Retention period, from which `expires_at` is derived.
    pub ttl: Duration,
    /// `--max-size` cap in bytes, applied to the measured running total.
    pub max_size: Option<u64>,
}

/// What a move left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveOutcome {
    /// The session exactly as the manifest on disk now describes it.
    pub session: QuarantineSession,
    /// The plan, with each item's outcome and the byte counters filled in.
    pub plan: CleanPlan,
}

/// Move every approved item into a new session of the store.
///
/// `quarantined_bytes` counts what was actually moved, measured rather than
/// planned; `reclaimed_bytes` is left as the plan had it, because a quarantined
/// item still occupies the disk (`AGENTS.md` §2.7).
///
/// # Errors
///
/// Only for failures of the store itself: the root cannot be read, the session
/// directory cannot be created, the manifest cannot be written, or the token and
/// the plan it carries do not describe the same work. The failure of a single
/// *item* is recorded in the manifest and in the plan, and the move goes on.
pub fn quarantine_items(
    token: &Approved<Write>,
    request: &MoveRequest,
    fs: &dyn FileOps,
    clock: &dyn Clock,
) -> Result<MoveOutcome, BrozaError> {
    let plan = token.plan().clone();
    let context = Context::new(request, &plan, token.items(), fs)?;
    fs.create_dir_all(&context.dir)?;
    let session = new_session(&plan, &context, clock, request.ttl)?;
    let progress = Progress { manifest: Manifest::new(session), plan, moved_bytes: 0 };
    manifest::write(fs, &context.manifest, &progress.manifest)?;
    let moved =
        token.items().iter().enumerate().try_fold(progress, |progress, (position, item)| {
            move_one(progress, position, item, &context, fs)
        })?;
    finish(moved, &context, fs)
}

/// Everything the loop needs that does not change between items.
struct Context {
    /// Directory of the session.
    dir: PathBuf,
    /// Manifest of the session.
    manifest: PathBuf,
    /// Device the store sits on.
    root_device: u64,
    /// `--max-size` cap.
    max_size: Option<u64>,
    /// Index in the plan of each approved item, in approval order.
    plan_indices: Vec<usize>,
}

impl Context {
    fn new(
        request: &MoveRequest,
        plan: &CleanPlan,
        approved: &[ApprovedItem],
        fs: &dyn FileOps,
    ) -> Result<Self, BrozaError> {
        let dir = layout::session_dir(&request.root, plan.session_id());
        Ok(Self {
            manifest: layout::manifest_path(&dir),
            dir,
            root_device: fs.metadata(&request.root)?.device,
            max_size: request.max_size,
            plan_indices: plan_indices(plan, approved)?,
        })
    }
}

/// The manifest, the plan and the running total, carried through the loop.
struct Progress {
    /// The manifest as it was last written.
    manifest: Manifest,
    /// The plan with the outcomes recorded so far.
    plan: CleanPlan,
    /// Bytes moved so far, against which `--max-size` is checked.
    moved_bytes: u64,
}

/// One item: attempt the move, record it, and persist the manifest.
fn move_one(
    progress: Progress,
    position: usize,
    item: &ApprovedItem,
    context: &Context,
    fs: &dyn FileOps,
) -> Result<Progress, BrozaError> {
    let destination = Destination {
        session_dir: &context.dir,
        sequence: sequence_of(position)?,
        root_device: context.root_device,
        max_size: context.max_size,
    };
    let attempt = attempt_move(item, &destination, progress.moved_bytes, fs);
    let Progress { manifest, plan, moved_bytes } = progress;
    let Manifest { manifest_version, session } = manifest;
    let entry = session
        .entries
        .get(position)
        .ok_or_else(|| desynchronised(&format!("the manifest has no entry {position}")))?;
    let session = with_entry(&session, position, &updated_entry(entry, &attempt));
    let index = *context
        .plan_indices
        .get(position)
        .ok_or_else(|| desynchronised(&format!("no plan item for approved item {position}")))?;
    let (status, error) = outcome_of(&attempt);
    let progress = Progress {
        manifest: Manifest { manifest_version, session },
        plan: plan.with_item_status(index, status, error)?,
        moved_bytes: moved_bytes.saturating_add(moved_of(&attempt)),
    };
    manifest::write(fs, &context.manifest, &progress.manifest)?;
    Ok(progress)
}

/// Close the session: mark it `complete` and fill in the plan's counters.
fn finish(progress: Progress, context: &Context, fs: &dyn FileOps) -> Result<MoveOutcome, BrozaError> {
    let Progress { manifest, plan, moved_bytes } = progress;
    let Manifest { manifest_version, session } = manifest;
    let manifest = Manifest {
        manifest_version,
        session: QuarantineSession { state: SessionState::Complete, ..session },
    };
    manifest::write(fs, &context.manifest, &manifest)?;
    let holds_items = manifest.session.entries.iter().any(|entry| entry.status == ItemStatus::Quarantined);
    let reclaimed_bytes = plan.reclaimed_bytes();
    let plan = plan
        .into_applied(holds_items.then(|| context.dir.clone()))?
        .with_bytes(moved_bytes, reclaimed_bytes)?;
    Ok(MoveOutcome { session: manifest.session, plan })
}

/// A fresh session: one planned entry per approved item, nothing moved yet.
fn new_session(
    plan: &CleanPlan,
    context: &Context,
    clock: &dyn Clock,
    retention: Duration,
) -> Result<QuarantineSession, BrozaError> {
    let created_at = clock.now();
    let entries = planned_entries(plan, &context.plan_indices)?;
    Ok(QuarantineSession {
        id: plan.session_id().clone(),
        created_at,
        expires_at: ttl::expires_at(created_at, retention),
        total_bytes: 0,
        item_count: u64::try_from(entries.len()).unwrap_or(u64::MAX),
        state: SessionState::InProgress,
        entries,
    })
}

/// Where each approved item sits in the plan.
///
/// The guard hands over evidence only for the paths that survived its checks,
/// in plan order, so the two lists are matched by walking them together. A path
/// that cannot be matched is a bug in the caller, not an item failure: pairing
/// one path's evidence with another path is exactly what the token prevents.
fn plan_indices(plan: &CleanPlan, approved: &[ApprovedItem]) -> Result<Vec<usize>, BrozaError> {
    let items = plan.items();
    let mut cursor = 0_usize;
    let mut indices = Vec::with_capacity(approved.len());
    for item in approved {
        let offset = items
            .get(cursor..)
            .unwrap_or_default()
            .iter()
            .position(|planned| planned.path == item.path() && planned.status == ItemStatus::Planned)
            .ok_or_else(|| desynchronised(&format!("no planned item for `{}`", item.path().display())))?;
        indices.push(cursor + offset);
        cursor += offset + 1;
    }
    Ok(indices)
}

/// A plan and a token that do not describe the same work.
fn desynchronised(reason: &str) -> BrozaError {
    BrozaError::Other(format!("quarantine move: {reason}"))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::{MoveOutcome, MoveRequest, quarantine_items};
    use crate::model::{ItemErrorCode, ItemStatus, QuarantineEntry, SessionState};
    use crate::ports::FileOps;
    use crate::quarantine::codes::{changed_since_check, max_size_exceeded};
    use crate::quarantine::fixtures::{NOW, ROOT, approved_write, at, session_dir, store_fs};
    use crate::quarantine::manifest;
    use crate::testing::{FakeFileOps, FixedClock};

    /// A cache file inside the home of the fake user.
    const CACHE: &str = "/Users/dana/Library/Caches/app.cache";
    /// A second cache file, so the order of the entries can be asserted.
    const OTHER: &str = "/Users/dana/Library/Caches/other.cache";
    /// A cache directory, whose real size only the store measures.
    const DERIVED: &str = "/Users/dana/Library/Caches/DerivedData";
    /// A path on the external volume, which the store cannot reach by rename.
    const EXTERNAL: &str = "/Volumes/External/.Trashes/501/old.dmg";
    /// The default retention period.
    const TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

    fn request(max_size: Option<u64>) -> MoveRequest {
        MoveRequest { root: PathBuf::from(ROOT), ttl: TTL, max_size }
    }

    fn moved(fs: &FakeFileOps, paths: &[(&str, u64)], max_size: Option<u64>) -> MoveOutcome {
        let token = approved_write(fs, paths);
        let clock = FixedClock::at(at(NOW));
        quarantine_items(&token, &request(max_size), fs, &clock).unwrap_or_else(|error| panic!("{error}"))
    }

    fn entry_of<'a>(outcome: &'a MoveOutcome, original: &str) -> &'a QuarantineEntry {
        outcome
            .session
            .entries
            .iter()
            .find(|entry| entry.original_path == Path::new(original))
            .unwrap_or_else(|| panic!("no entry for {original}"))
    }

    #[test]
    fn every_item_is_renamed_into_its_own_numbered_directory() {
        let fs = store_fs().with_sized_file(CACHE, 10).with_sized_file(OTHER, 20);

        let outcome = moved(&fs, &[(CACHE, 10), (OTHER, 20)], None);

        let dir = session_dir();
        assert_eq!(entry_of(&outcome, CACHE).stored_path, Some(dir.join("items/0001/app.cache")));
        assert_eq!(entry_of(&outcome, OTHER).stored_path, Some(dir.join("items/0002/other.cache")));
        assert!(fs.exists(&dir.join("items/0001/app.cache")));
        assert!(!fs.exists(Path::new(CACHE)), "the source is gone, not copied");
        assert_eq!(outcome.session.state, SessionState::Complete);
        assert_eq!(outcome.session.total_bytes, 30);
        assert_eq!(outcome.session.item_count, 2);
    }

    #[test]
    fn the_plan_counts_quarantined_bytes_and_never_reclaims_them() {
        let fs = store_fs().with_sized_file(CACHE, 10);

        let outcome = moved(&fs, &[(CACHE, 10)], None);

        assert_eq!(outcome.plan.quarantined_bytes(), 10);
        assert_eq!(outcome.plan.reclaimed_bytes(), 0, "a quarantined item still occupies the disk");
        assert_eq!(outcome.plan.quarantine_path(), Some(&session_dir()));
        assert_eq!(outcome.plan.items()[0].status, ItemStatus::Quarantined);
        assert!(!outcome.plan.is_dry_run());
    }

    #[test]
    fn the_session_expires_one_retention_period_after_it_was_created() {
        let fs = store_fs().with_sized_file(CACHE, 10);

        let outcome = moved(&fs, &[(CACHE, 10)], None);

        assert_eq!(outcome.session.created_at, at(NOW));
        assert_eq!(outcome.session.expires_at, at("2026-10-21T10:36:08Z"));
    }

    #[test]
    fn an_item_on_another_volume_is_skipped_and_left_alone() {
        let fs = store_fs().with_sized_file(CACHE, 10).with_sized_file(EXTERNAL, 60);

        let outcome = moved(&fs, &[(CACHE, 10), (EXTERNAL, 60)], None);

        let skipped = entry_of(&outcome, EXTERNAL);
        assert_eq!(skipped.status, ItemStatus::Skipped);
        assert_eq!(skipped.error, Some(ItemErrorCode::CrossVolume));
        assert!(skipped.stored_path.is_none());
        assert!(fs.exists(Path::new(EXTERNAL)), "a cross-volume item is never touched");
        assert_eq!(outcome.plan.quarantined_bytes(), 10);
    }

    #[test]
    fn an_item_that_changed_since_the_check_fails_instead_of_being_moved() {
        let fs = store_fs().with_sized_file(CACHE, 10).with_sized_file(OTHER, 20);
        let token = approved_write(&fs, &[(CACHE, 10), (OTHER, 20)]);
        fs.remove_tree(Path::new(CACHE)).unwrap_or_else(|error| panic!("{error}"));
        fs.add_file(CACHE, b"an impostor");

        let outcome = quarantine_items(&token, &request(None), &fs, &FixedClock::at(at(NOW)))
            .unwrap_or_else(|error| panic!("{error}"));

        let failed = entry_of(&outcome, CACHE);
        assert_eq!(failed.status, ItemStatus::Failed);
        assert_eq!(failed.error, Some(changed_since_check()));
        assert!(fs.exists(Path::new(CACHE)), "the impostor is left exactly where it was");
        assert_eq!(entry_of(&outcome, OTHER).status, ItemStatus::Quarantined, "the run goes on");
    }

    #[test]
    fn an_item_that_vanished_since_the_check_fails_with_not_found() {
        let fs = store_fs().with_sized_file(CACHE, 10);
        let token = approved_write(&fs, &[(CACHE, 10)]);
        fs.remove_tree(Path::new(CACHE)).unwrap_or_else(|error| panic!("{error}"));

        let outcome = quarantine_items(&token, &request(None), &fs, &FixedClock::at(at(NOW)))
            .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(entry_of(&outcome, CACHE).status, ItemStatus::Failed);
        assert_eq!(entry_of(&outcome, CACHE).error, Some(ItemErrorCode::NotFound));
        assert_eq!(outcome.plan.quarantined_bytes(), 0);
        assert_eq!(outcome.plan.quarantine_path(), None, "an empty session has no path to report");
    }

    #[test]
    fn a_directory_is_measured_again_before_it_is_moved() {
        let fs = store_fs()
            .with_sized_file(format!("{DERIVED}/a"), 1_000)
            .with_sized_file(format!("{DERIVED}/deep/b"), 500);

        let outcome = moved(&fs, &[(DERIVED, 100)], None);

        assert_eq!(entry_of(&outcome, DERIVED).size_bytes, 1_500, "the scan's figure was stale");
        assert_eq!(outcome.plan.quarantined_bytes(), 1_500);
        assert_eq!(outcome.plan.planned_bytes(), 100, "the plan still reports what was planned");
    }

    #[test]
    fn an_item_that_would_pass_the_cap_is_skipped_and_the_smaller_ones_still_move() {
        let fs = store_fs().with_sized_file(CACHE, 10).with_sized_file(format!("{DERIVED}/a"), 1_000);

        let outcome = moved(&fs, &[(CACHE, 10), (DERIVED, 10)], Some(100));

        assert_eq!(entry_of(&outcome, CACHE).status, ItemStatus::Quarantined);
        assert_eq!(entry_of(&outcome, DERIVED).status, ItemStatus::Skipped);
        assert_eq!(entry_of(&outcome, DERIVED).error, Some(max_size_exceeded()));
        assert!(fs.exists(Path::new(DERIVED)));
        assert_eq!(outcome.plan.quarantined_bytes(), 10);
    }

    #[test]
    fn the_manifest_on_disk_matches_what_the_move_reports() {
        let fs = store_fs().with_sized_file(CACHE, 10).with_sized_file(EXTERNAL, 60);

        let outcome = moved(&fs, &[(CACHE, 10), (EXTERNAL, 60)], None);

        let read = manifest::read(&fs, &session_dir().join("manifest.json"))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(read.session, outcome.session);
        assert_eq!(read.session.entries.len(), 2, "a skipped item is recorded, not dropped");
    }

    #[test]
    fn a_source_broza_may_not_read_fails_the_item_and_not_the_run() {
        let fs = store_fs().with_sized_file(CACHE, 10).with_sized_file(OTHER, 20);
        let token = approved_write(&fs, &[(CACHE, 10), (OTHER, 20)]);
        fs.add_denied(CACHE);

        let outcome = quarantine_items(&token, &request(None), &fs, &FixedClock::at(at(NOW)))
            .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(entry_of(&outcome, CACHE).error, Some(ItemErrorCode::PermissionDenied));
        assert_eq!(entry_of(&outcome, OTHER).status, ItemStatus::Quarantined);
    }

    #[test]
    fn a_store_root_that_cannot_be_read_stops_the_whole_move() {
        let fs = store_fs().with_sized_file(CACHE, 10);
        let token = approved_write(&fs, &[(CACHE, 10)]);
        let missing = MoveRequest { root: PathBuf::from("/Users/dana/ghost"), ttl: TTL, max_size: None };

        let error = quarantine_items(&token, &missing, &fs, &FixedClock::at(at(NOW)));

        assert!(error.is_err(), "a store Broza cannot stat is not a per-item failure");
        assert!(fs.exists(Path::new(CACHE)));
    }
}
