//! Tests for [`super::restore`]: putting a session, or part of one, back.
use std::path::{Path, PathBuf};

use super::restore::{SESSION_LEFT_BEHIND, restore_entries, restore_session};
use super::selection::{Wanted, group_by_session, restore_order};
use crate::BrozaError;
use crate::model::{
    EntryId, ItemErrorCode, ItemStatus, OperationKind, QuarantineSession, RestoreReport, SessionId,
    SessionState,
};
use crate::ports::FileOps;
use crate::quarantine::fixtures::{
    ROOT, entry, quarantine_write, quarantined, restore_targets, session_dir, session_id, store_fs,
    writable_paths,
};
use crate::quarantine::report::Reported;
use crate::quarantine::store;
use crate::testing::FakeFileOps;

const CACHE: &str = "/Users/dana/Library/Caches/app.cache";
const OTHER: &str = "/Users/dana/Library/Caches/other.cache";

fn two_items() -> (FakeFileOps, QuarantineSession) {
    let fs = store_fs().with_sized_file(CACHE, 10).with_sized_file(OTHER, 20);
    let session = quarantined(&fs, &[(CACHE, 10), (OTHER, 20)]);
    (fs, session)
}

fn restored(fs: &FakeFileOps, session: &QuarantineSession, to: Option<&Path>) -> Reported<RestoreReport> {
    let sources = quarantine_write(fs, &writable_paths(session));
    let targets = restore_targets(fs, session, to);
    restore_session(&sources, &targets, &session_id(), Path::new(ROOT), fs, to)
        .unwrap_or_else(|error| panic!("{error}"))
}

fn entry_id(sequence: u32) -> EntryId {
    crate::quarantine::layout::entry_id(&session_id(), sequence).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn a_restored_session_puts_every_item_back_and_disappears() {
    let (fs, session) = two_items();

    let reported = restored(&fs, &session, None);

    assert_eq!(reported.data.operation, OperationKind::Restore);
    assert_eq!(reported.data.restored_bytes, 2 * 4096, "allocated: one block per file");
    assert_eq!(reported.data.sessions[0].status, ItemStatus::Restored);
    assert!(fs.exists(Path::new(CACHE)) && fs.exists(Path::new(OTHER)));
    assert!(!fs.exists(&session_dir()), "an emptied session is deleted");
    assert!(!reported.is_partial());
}

#[test]
fn a_restored_item_reports_where_it_went_and_no_longer_has_a_stored_path() {
    let (fs, session) = two_items();

    let reported = restored(&fs, &session, None);

    let items = &reported.data.sessions[0].items;
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].restored_to, Some(PathBuf::from(CACHE)));
    assert!(items[0].stored_path.is_none());
    assert_eq!(items[0].id.sequence_part(), "0001", "the report is in sequence order");
}

#[test]
fn an_occupied_original_path_is_skipped_and_its_session_survives() {
    let (fs, session) = two_items();
    fs.add_file(CACHE, b"something new");

    let reported = restored(&fs, &session, None);

    assert_eq!(reported.data.sessions[0].status, ItemStatus::Skipped);
    assert_eq!(reported.data.restored_bytes, 4096, "the other item still went back");
    assert_eq!(reported.errors.len(), 1);
    assert_eq!(reported.errors[0].code, ItemErrorCode::Collision.to_string());
    assert!(reported.is_partial(), "a skipped entry means exit 5");
    let left = store::read_one(&fs, Path::new(ROOT), &session_id()).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(left.session().state, SessionState::Restoring, "a partial restore can be retried");
    assert_eq!(left.session().total_bytes, 4096, "only the skipped item is still held");
}

#[test]
fn restoring_to_another_directory_never_touches_the_original_paths() {
    let (fs, session) = two_items();
    let elsewhere = PathBuf::from("/Users/dana/Rescued");

    let reported = restored(&fs, &session, Some(&elsewhere));

    assert_eq!(reported.data.restored_bytes, 2 * 4096, "allocated: one block per file");
    assert!(fs.exists(&elsewhere.join("0001_app.cache")));
    assert!(fs.exists(&elsewhere.join("0002_other.cache")));
    assert!(!fs.exists(Path::new(CACHE)));
}

#[test]
fn restoring_one_entry_leaves_the_rest_of_the_session_alone() {
    let (fs, session) = two_items();
    let sources = quarantine_write(&fs, &writable_paths(&session));
    let targets = restore_targets(&fs, &session, None);

    let reported = restore_entries(&sources, &targets, &[entry_id(1)], Path::new(ROOT), &fs, None)
        .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.sessions[0].items.len(), 1);
    assert_eq!(reported.data.restored_bytes, 4096);
    assert!(fs.exists(Path::new(CACHE)) && !fs.exists(Path::new(OTHER)));
    let left = store::read_one(&fs, Path::new(ROOT), &session_id()).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(left.session().state, SessionState::Complete);
    assert_eq!(left.session().entries[0].status, ItemStatus::Restored);
    assert_eq!(left.session().entries[1].status, ItemStatus::Quarantined);
}

#[test]
fn a_session_whose_directory_was_not_approved_is_emptied_and_reported() {
    let (fs, session) = two_items();
    let stored_only: Vec<PathBuf> =
        session.entries.iter().filter_map(|entry| entry.stored_path.clone()).collect();
    let sources = quarantine_write(&fs, &stored_only);
    let targets = restore_targets(&fs, &session, None);

    let reported = restore_session(&sources, &targets, &session_id(), Path::new(ROOT), &fs, None)
        .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.sessions[0].status, ItemStatus::Restored);
    assert_eq!(reported.warnings[0].code, SESSION_LEFT_BEHIND);
    assert!(fs.exists(&session_dir()), "an unapproved directory is never removed");
    assert!(!reported.is_partial(), "the items did go back; only the empty shell stayed");
}

#[test]
fn an_unknown_session_is_a_missing_target_before_anything_is_written() {
    let (fs, _session) = two_items();
    let ghost: SessionId = "cln_20260801091200_c3d4".parse().unwrap_or_else(|e| panic!("{e}"));

    let error = crate::quarantine::selection::session_destinations(&fs, Path::new(ROOT), &ghost, None);

    assert!(matches!(error, Err(BrozaError::TargetNotFound(_))), "{error:?}");
}

#[test]
fn a_session_that_cannot_be_read_is_reported_and_the_run_goes_on() {
    let (fs, session) = two_items();
    let sources = quarantine_write(&fs, &writable_paths(&session));
    let targets = restore_targets(&fs, &session, None);
    let ghost: SessionId = "cln_20260801091200_c3d4".parse().unwrap_or_else(|e| panic!("{e}"));

    let reported = restore_session(&sources, &targets, &ghost, Path::new(ROOT), &fs, None)
        .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.sessions[0].status, ItemStatus::Failed);
    assert_eq!(reported.errors[0].code, store::MANIFEST_CORRUPT);
    assert!(reported.is_partial(), "exit 5, never an abort in the middle of a list");
}

#[test]
fn entries_go_back_in_reverse_sequence_order() {
    let entries = vec![
        entry(1, "/Users/dana/a", 1, ItemStatus::Quarantined),
        entry(2, "/Users/dana/a/child", 1, ItemStatus::Quarantined),
        entry(3, "/Users/dana/b", 1, ItemStatus::Skipped),
    ];
    let wanted = Wanted::whole(&session_id());

    let order = restore_order(entries, &wanted);

    assert_eq!(order, vec![entry_id(2), entry_id(1)], "a skipped entry has nothing to put back");
}

#[test]
fn identifiers_are_grouped_by_the_session_they_name() {
    let other: EntryId = "cln_20260801091200_c3d4/0009".parse().unwrap_or_else(|e| panic!("{e}"));
    let ids = vec![entry_id(1), other.clone(), entry_id(2)];

    let grouped = group_by_session(&ids).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(grouped.len(), 2);
    assert_eq!(grouped[0].session, session_id());
    assert_eq!(grouped[0].entries, Some(vec![entry_id(1), entry_id(2)]));
    assert_eq!(grouped[1].entries, Some(vec![other]));
}
