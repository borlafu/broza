//! Tests for [`super::expiry`]: which sessions go, and which are refused.
use std::path::Path;
use std::time::Duration;

use super::expiry::{all_sessions, expire, expired_sessions, purge};
use crate::BrozaError;
use crate::model::{ItemStatus, OperationKind, SessionId, SessionState};
use crate::ports::FileOps;
use crate::quarantine::fixtures::{
    NOW, ROOT, at, entry, manifest_file, session, session_dir, session_fs, session_id,
};
use crate::quarantine::manifest::{self, Manifest};
use crate::quarantine::store;
use crate::safety::guard::{Approved, QuarantineWrite, approve_quarantine_write};
use crate::testing::{FakeFileOps, FixedClock, mac_mount_table};

const TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const AFTER_TTL: &str = "2026-11-01T00:00:00Z";

fn write_session(fs: &FakeFileOps, state: SessionState) {
    fs.add_dir(session_dir());
    let entries = vec![
        entry(1, "/Users/dana/Library/Caches/app", 10, ItemStatus::Quarantined),
        entry(2, "/Volumes/External/x", 60, ItemStatus::Skipped),
    ];
    let manifest = Manifest::new(session(state, entries));
    manifest::write(fs, &manifest_file(), &manifest).unwrap_or_else(|error| panic!("{error}"));
    fs.add_file(session_dir().join("items/0001/app"), b"content");
}

fn stored(state: SessionState) -> FakeFileOps {
    let fs = session_fs();
    write_session(&fs, state);
    fs
}

fn token(fs: &FakeFileOps) -> Approved<QuarantineWrite> {
    approve_quarantine_write(&[session_dir()], Path::new(ROOT), &mac_mount_table(), fs)
        .unwrap_or_else(|error| panic!("{error}"))
}

fn ids() -> Vec<SessionId> {
    vec![session_id()]
}

#[test]
fn only_a_session_past_its_retention_period_expires() {
    let fs = stored(SessionState::Complete);
    let before = FixedClock::at(at(NOW));
    let after = FixedClock::at(at(AFTER_TTL));

    assert_eq!(expired_sessions(Path::new(ROOT), &fs, &before, TTL).ok(), Some(Vec::new()));
    assert_eq!(expired_sessions(Path::new(ROOT), &fs, &after, TTL).ok(), Some(ids()));
}

#[test]
fn a_session_an_interrupted_move_left_behind_still_expires() {
    let fs = stored(SessionState::InProgress);
    let after = FixedClock::at(at(AFTER_TTL));

    assert_eq!(
        expired_sessions(Path::new(ROOT), &fs, &after, TTL).ok(),
        Some(ids()),
        "reading it settled what it holds, so it can be reclaimed"
    );
}

#[test]
fn a_session_being_restored_never_expires_on_its_own() {
    let fs = stored(SessionState::Restoring);
    let after = FixedClock::at(at(AFTER_TTL));

    assert_eq!(expired_sessions(Path::new(ROOT), &fs, &after, TTL).ok(), Some(Vec::new()));
    assert_eq!(all_sessions(Path::new(ROOT), &fs).ok(), Some(ids()), "but `purge --all` still sees it");
}

#[test]
fn expiring_a_session_removes_its_whole_directory_and_frees_its_bytes() {
    let fs = stored(SessionState::Complete);

    let reported =
        expire(&token(&fs), &ids(), Path::new(ROOT), &fs).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.operation, OperationKind::Expire);
    assert_eq!(reported.data.reclaimed_bytes, 10, "only the entries it held count");
    assert_eq!(reported.data.sessions[0].status, ItemStatus::Purged);
    assert!(!fs.exists(&session_dir()));
    assert!(fs.exists(Path::new(ROOT)), "the store itself stays");
    assert!(!reported.is_partial());
}

#[test]
fn purging_uses_the_same_shape_with_its_own_operation() {
    let fs = stored(SessionState::Complete);

    let reported = purge(&token(&fs), &ids(), Path::new(ROOT), &fs).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.operation, OperationKind::Purge);
    assert_eq!(reported.data.sessions[0].item_count, 2);
    assert!(!fs.exists(&session_dir()));
}

#[test]
fn a_session_being_restored_is_refused_and_reported() {
    let fs = stored(SessionState::Restoring);

    let reported = purge(&token(&fs), &ids(), Path::new(ROOT), &fs).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.sessions[0].status, ItemStatus::Skipped);
    assert_eq!(reported.data.reclaimed_bytes, 0);
    assert_eq!(reported.errors[0].code, "session_busy");
    assert!(reported.is_partial(), "a refused session means exit 5");
    assert!(fs.exists(&session_dir()));
}

#[test]
fn expiry_refuses_a_session_holding_items_nobody_listed() {
    let fs = stored(SessionState::Complete);
    fs.add_file(session_dir().join("items/0009/mystery"), b"unaccounted");

    let reported =
        expire(&token(&fs), &ids(), Path::new(ROOT), &fs).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.sessions[0].status, ItemStatus::Skipped);
    assert!(reported.errors.iter().any(|entry| entry.code == store::ORPHANED_ITEM), "{reported:?}");
    assert!(fs.exists(&session_dir().join("items/0009/mystery")), "nothing unattended is deleted");
}

#[test]
fn purge_removes_an_unlisted_item_with_its_session_and_says_so() {
    let fs = stored(SessionState::Complete);
    fs.add_file(session_dir().join("items/0009/mystery"), b"unaccounted");

    let reported = purge(&token(&fs), &ids(), Path::new(ROOT), &fs).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.sessions[0].status, ItemStatus::Purged);
    assert_eq!(reported.warnings.len(), 1);
    assert_eq!(reported.warnings[0].code, store::ORPHANED_ITEM);
    assert!(!fs.exists(&session_dir()));
}

#[test]
fn a_session_directory_that_changed_since_the_check_is_left_alone() {
    let fs = stored(SessionState::Complete);
    let approval = token(&fs);
    fs.remove_tree(&session_dir()).unwrap_or_else(|error| panic!("{error}"));
    write_session(&fs, SessionState::Complete);

    let reported = purge(&approval, &ids(), Path::new(ROOT), &fs).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.sessions[0].status, ItemStatus::Skipped);
    assert!(reported.is_partial());
    assert!(fs.exists(&session_dir()), "a directory that is not the approved one is never removed");
}

#[test]
fn an_unknown_session_is_reported_and_never_stops_the_rest() {
    let fs = stored(SessionState::Complete);
    let ghost = "cln_20260801091200_c3d4".parse().unwrap_or_else(|error| panic!("{error}"));

    let reported = purge(&token(&fs), &[ghost, session_id()], Path::new(ROOT), &fs)
        .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.sessions[0].status, ItemStatus::Skipped);
    assert_eq!(reported.errors[0].code, store::MANIFEST_CORRUPT);
    assert_eq!(reported.data.sessions[1].status, ItemStatus::Purged, "the known one still goes");
}

#[test]
fn a_corrupt_session_is_skipped_rather_than_deleted_blind() {
    let fs = stored(SessionState::Complete);
    fs.add_file(manifest_file(), b"{ truncated");

    let reported =
        expire(&token(&fs), &ids(), Path::new(ROOT), &fs).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.sessions[0].status, ItemStatus::Skipped);
    assert_eq!(reported.errors[0].code, store::MANIFEST_CORRUPT);
    assert!(reported.is_partial());
    assert!(fs.exists(&session_dir()));
}

#[test]
fn a_session_the_token_does_not_cover_stops_the_operation() {
    let fs = stored(SessionState::Complete);
    let empty = approve_quarantine_write(&[], Path::new(ROOT), &mac_mount_table(), &fs)
        .unwrap_or_else(|error| panic!("{error}"));

    let error = purge(&empty, &ids(), Path::new(ROOT), &fs);

    assert!(matches!(error, Err(BrozaError::Other(_))), "{error:?}");
    assert!(fs.exists(&session_dir()));
}
