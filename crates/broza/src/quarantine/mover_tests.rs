//! Tests for [`super::mover`]: what a move leaves in the store and in the plan.
use std::path::Path;
use std::time::Duration;

use super::mover::{MoveOutcome, MoveRequest, quarantine_items};
use crate::model::{ItemErrorCode, ItemStatus, QuarantineEntry, SessionState};
use crate::ports::FileOps;
use crate::quarantine::codes::{changed_since_check, max_size_exceeded};
use crate::quarantine::fixtures::{
    NOW, ROOT, approved_without_store, approved_write, at, session_dir, store_fs,
};
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
    MoveRequest { ttl: TTL, max_size }
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
fn a_directory_is_measured_again_when_a_cap_makes_the_difference_matter() {
    let fs = store_fs()
        .with_sized_file(format!("{DERIVED}/a"), 1_000)
        .with_sized_file(format!("{DERIVED}/deep/b"), 500);

    let outcome = moved(&fs, &[(DERIVED, 100)], Some(1_000_000));

    assert_eq!(entry_of(&outcome, DERIVED).size_bytes, 2 * 4096, "the scan's figure was stale");
    assert_eq!(outcome.plan.quarantined_bytes(), 2 * 4096);
    assert_eq!(outcome.plan.planned_bytes(), 100, "the plan still reports what was planned");
}

#[test]
fn a_directory_is_not_walked_when_no_cap_depends_on_its_size() {
    let fs = store_fs()
        .with_sized_file(format!("{DERIVED}/a"), 1_000)
        .with_sized_file(format!("{DERIVED}/deep/b"), 500);

    let outcome = moved(&fs, &[(DERIVED, 100)], None);

    assert_eq!(
        entry_of(&outcome, DERIVED).size_bytes,
        100,
        "without a cap the scan's figure is good enough and the walk is skipped"
    );
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

    let read =
        manifest::read(&fs, &session_dir().join("manifest.json")).unwrap_or_else(|error| panic!("{error}"));
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
fn a_run_with_no_validated_store_moves_nothing() {
    let fs = store_fs().with_sized_file(CACHE, 10);
    let token = approved_without_store(&fs, &[(CACHE, 10)]);

    let error = quarantine_items(&token, &request(None), &fs, &FixedClock::at(at(NOW)));

    assert!(error.is_err(), "there is nowhere the guard said Broza may write");
    assert!(fs.exists(Path::new(CACHE)));
}

#[test]
fn the_store_comes_from_the_token_and_not_from_the_caller() {
    let fs = store_fs().with_sized_file(CACHE, 10);

    let outcome = moved(&fs, &[(CACHE, 10)], None);

    let inside = outcome.plan.quarantine_path().cloned().unwrap_or_default();
    assert!(inside.starts_with(ROOT), "{}", inside.display());
}
