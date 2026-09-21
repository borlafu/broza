//! Adversarial probes against the quarantine store: locking and exclusive filesystem operations.
//!
//! Round-2 findings of the M3-A review: two Brozas at once, a filesystem
//! without `renamex_np`, and a lock file this Broza cannot open. Nothing is
//! removed that another run is writing, and one unreadable session never hides
//! or aborts the others.
#![cfg(feature = "test-support")]

mod quarantine_world;

use std::path::{Path, PathBuf};
use std::time::Duration;

use broza::adapters::StdFileOps;
use broza::model::{ItemStatus, QuarantineSession, SessionId};
use broza::ports::{FileOps, already_exists};
use broza::quarantine::{expiry, layout, list, lock, report, restore, store};
use broza::safety::guard::{RestoreRequest, approve_restore_targets};
use broza::testing::mac_mount_table;
use quarantine_world::{A_DAY, CACHE, HOME, ROOT, TTL, clock, quarantine, restore_token, store_token, tree};

/// The stored path of the first entry of `session`.
fn first_stored(session: &QuarantineSession) -> PathBuf {
    session.entries[0].stored_path.clone().unwrap_or_else(|| panic!("nothing was moved"))
}

/// R2 HIGH 1 — a purge cannot delete a session another Broza is filling.
#[test]
fn a_purge_that_interleaves_with_a_move_is_told_the_session_is_busy() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let dir = layout::session_dir(Path::new(ROOT), &outcome.session.id);
    let token = store_token(&fs, &outcome.session);
    // What the store looks like while a move is still running.
    let _moving = lock::take(&fs, &dir).into_result().unwrap_or_else(|error| panic!("{error}"));

    let reported = expiry::purge(&token, std::slice::from_ref(&outcome.session.id), Path::new(ROOT), &fs)
        .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.sessions[0].status, ItemStatus::Skipped);
    assert_eq!(reported.data.reclaimed_bytes, 0);
    assert_eq!(reported.errors[0].code, "session_busy");
    assert!(fs.exists(&dir), "the session is intact");
    assert!(fs.exists(&first_stored(&outcome.session)), "and so is the file inside it");
}

/// R2 HIGH 1 — nor can a restore, and nothing of it is moved back.
#[test]
fn a_restore_of_a_busy_session_moves_nothing() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let dir = layout::session_dir(Path::new(ROOT), &outcome.session.id);
    let sources = store_token(&fs, &outcome.session);
    let targets = restore_token(&fs, &outcome.session.id, None);
    let _moving = lock::take(&fs, &dir).into_result().unwrap_or_else(|error| panic!("{error}"));

    let reported =
        restore::restore_session(&sources, &targets, &outcome.session.id, Path::new(ROOT), &fs, None)
            .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.restored_bytes, 0);
    assert_eq!(reported.data.sessions[0].status, ItemStatus::Skipped);
    assert_eq!(reported.errors[0].code, "session_busy");
    assert!(!fs.exists(Path::new(CACHE)), "nothing went back");
    assert!(fs.exists(&first_stored(&outcome.session)));
}

/// R2 HIGH 1 — a zero retention period does not sweep away a running move.
#[test]
fn a_session_being_written_never_expires_however_short_the_retention_is() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let dir = layout::session_dir(Path::new(ROOT), &outcome.session.id);
    at.advance(A_DAY);

    let while_free = expiry::expired_sessions(Path::new(ROOT), &fs, &at, Duration::ZERO)
        .unwrap_or_else(|error| panic!("{error}"));
    let _moving = lock::take(&fs, &dir).into_result().unwrap_or_else(|error| panic!("{error}"));
    let while_busy = expiry::expired_sessions(Path::new(ROOT), &fs, &at, Duration::ZERO)
        .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(while_free, vec![outcome.session.id.clone()], "with `quarantine-ttl 0` it is due");
    assert!(while_busy.is_empty(), "but not while a Broza is writing it");
}

/// R2 HIGH 1 — a listing shows a busy session instead of hiding or locking it.
#[test]
fn a_listing_reports_a_busy_session_without_writing_anything() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let dir = layout::session_dir(Path::new(ROOT), &outcome.session.id);
    let _moving = lock::take(&fs, &dir).into_result().unwrap_or_else(|error| panic!("{error}"));
    let before = fs.paths();

    let listed =
        list::list_sessions(Path::new(ROOT), &fs, &at, TTL).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(listed.data.sessions.len(), 1, "it is still shown");
    assert!(listed.warnings.iter().any(|warning| warning.code == list::SESSION_BUSY), "{listed:?}");
    assert!(!listed.is_partial(), "a warning, not a failure");
    assert_eq!(fs.paths(), before, "a listing writes nothing at all");
}

/// R2 HIGH 2 — a filesystem without `renamex_np` still refuses to overwrite.
#[test]
fn a_filesystem_without_an_exclusive_rename_checks_first_and_says_so() {
    let fs = tree();
    fs.deny_exclusive_rename(ROOT);
    let at = clock();

    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);

    assert_eq!(outcome.session.entries[0].status, ItemStatus::Quarantined);
    assert_eq!(outcome.warnings.len(), 1);
    assert_eq!(outcome.warnings[0].code, report::EXCLUSIVE_RENAME_UNSUPPORTED);
}

/// R2 HIGH 2 — and the fallback still refuses an occupied destination.
#[test]
fn the_fallback_rename_never_replaces_what_is_already_there() {
    let fs = tree();
    fs.deny_exclusive_rename("/Users/dana");
    fs.add_file("/Users/dana/to", b"old");
    fs.add_file("/Users/dana/from", b"new");

    let refused = fs.rename_exclusive(Path::new("/Users/dana/from"), Path::new("/Users/dana/to"));

    assert!(refused.is_err_and(|error| already_exists(&error)));
    assert_eq!(fs.read(Path::new("/Users/dana/to")).ok(), Some(b"old".to_vec()));
}

/// R2 MEDIUM 3 — purging reports the bytes the orphans occupied too.
#[test]
fn purging_counts_the_unlisted_items_it_freed() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let dir = layout::session_dir(Path::new(ROOT), &outcome.session.id);
    fs.add_file(dir.join("items/0099/mystery"), b"unaccounted for");

    let reported = expiry::purge(
        &store_token(&fs, &outcome.session),
        std::slice::from_ref(&outcome.session.id),
        Path::new(ROOT),
        &fs,
    )
    .unwrap_or_else(|error| panic!("{error}"));

    assert!(reported.data.reclaimed_bytes > 12, "{}", reported.data.reclaimed_bytes);
    assert_eq!(reported.warnings[0].code, store::ORPHANED_ITEM);
}

/// R2 MEDIUM 4 — a stray file keeps the session even outside `items/`.
#[test]
fn a_stray_file_beside_the_manifest_keeps_the_session() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let dir = layout::session_dir(Path::new(ROOT), &outcome.session.id);
    let sources = store_token(&fs, &outcome.session);
    let targets = restore_token(&fs, &outcome.session.id, None);
    fs.add_file(dir.join("notes.txt"), b"something a crash left");

    let reported =
        restore::restore_session(&sources, &targets, &outcome.session.id, Path::new(ROOT), &fs, None)
            .unwrap_or_else(|error| panic!("{error}"));

    assert!(fs.exists(Path::new(CACHE)), "the item did go back");
    assert!(fs.exists(&dir), "but the session is kept: something else is in it");
    assert_eq!(reported.errors[0].code, restore::SESSION_HAS_UNTRACKED_ITEMS);
}

/// R2 MEDIUM 5 — a restore never writes back into the store.
#[test]
fn an_alternative_directory_inside_the_store_is_refused() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let inside = PathBuf::from(ROOT).join("rescued");
    let wanted = restore::session_destinations(&fs, Path::new(ROOT), &outcome.session.id, Some(&inside))
        .unwrap_or_else(|error| panic!("{error}"));
    let request = RestoreRequest {
        to: Some(inside),
        quarantine_root: Some(PathBuf::from(ROOT)),
        ..RestoreRequest::new(HOME)
    };

    let refused = approve_restore_targets(&wanted, &request, &mac_mount_table(), &fs);

    assert!(refused.is_err(), "{refused:?}");
}

/// R2 HIGH 1 — the real filesystem's lock behaves like the fake's.
#[test]
fn a_real_session_lock_is_exclusive_across_two_holders() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let dir = temp.path().to_path_buf();

    let held = StdFileOps.lock_exclusive(&layout::lock_path(&dir));
    let second = StdFileOps.lock_exclusive(&layout::lock_path(&dir));

    assert!(held.is_ok());
    assert!(second.err().is_some_and(|error| broza::ports::is_busy(&error)));
    drop(held);
    assert!(StdFileOps.lock_exclusive(&layout::lock_path(&dir)).is_ok(), "released on drop");
}

/// A real filesystem claims a session name exactly the way the fake does.
#[test]
fn a_real_store_claims_its_session_name_exclusively() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let base = std::fs::canonicalize(temp.path()).unwrap_or_else(|error| panic!("{error}"));
    let root = base.join("quarantine");
    let id: SessionId = "cln_20260921103608_0000".parse().unwrap_or_else(|error| panic!("{error}"));
    StdFileOps.create_dir_all(&root).unwrap_or_else(|error| panic!("{error}"));
    let dir = layout::session_dir(&root, &id);

    StdFileOps.create_dir_exclusive(&dir).unwrap_or_else(|error| panic!("{error}"));
    let again = StdFileOps.create_dir_exclusive(&dir);
    let next = layout::session_dir(&root, &layout::next_session_id(&id).unwrap_or_else(|e| panic!("{e}")));

    assert!(again.is_err_and(|error| already_exists(&error)));
    assert!(StdFileOps.create_dir_exclusive(&next).is_ok(), "the next name is free");
}

/// R3 HIGH 1 — a lock file Broza cannot open is reported, never fatal.
#[test]
fn a_lock_that_cannot_be_opened_is_a_warning_in_the_listing() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let dir = layout::session_dir(Path::new(ROOT), &outcome.session.id);
    // A root-owned `.lock` left behind by `sudo broza`: present, unreadable.
    fs.add_file(layout::lock_path(&dir), b"");
    fs.add_denied(layout::lock_path(&dir));

    let listed =
        list::list_sessions(Path::new(ROOT), &fs, &at, TTL).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(listed.data.sessions.len(), 1, "the session is still shown");
    assert!(listed.warnings.iter().any(|warning| warning.code == list::LOCK_UNREADABLE), "{listed:?}");
    assert!(!listed.is_partial(), "a warning, not a failure");
}

/// R3 HIGH 1 — one session whose lock cannot be opened does not stop a purge of the others.
#[test]
fn purging_continues_past_a_session_whose_lock_cannot_be_opened() {
    let fs = tree();
    let at = clock();
    let stuck = quarantine(&fs, &at, &[(CACHE, 12)]);
    at.advance(A_DAY);
    fs.add_file(CACHE, b"another cache file");
    let healthy = quarantine(&fs, &at, &[(CACHE, 18)]);
    let stuck_dir = layout::session_dir(Path::new(ROOT), &stuck.session.id);
    fs.add_file(layout::lock_path(&stuck_dir), b"");
    fs.add_denied(layout::lock_path(&stuck_dir));
    let ids = vec![stuck.session.id.clone(), healthy.session.id.clone()];

    let reported = expiry::purge(&store_token(&fs, &healthy.session), &ids, Path::new(ROOT), &fs)
        .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.sessions[0].status, ItemStatus::Skipped, "{reported:?}");
    assert_eq!(reported.data.sessions[1].status, ItemStatus::Purged);
    assert!(fs.exists(&stuck_dir), "what cannot be locked is not deleted blind");
    assert!(reported.is_partial(), "exit 5, not exit 1");
}

/// R3 HIGH 1 — the unattended sweep never touches a session it cannot lock.
#[test]
fn a_session_whose_lock_cannot_be_opened_never_expires_automatically() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let dir = layout::session_dir(Path::new(ROOT), &outcome.session.id);
    fs.add_file(layout::lock_path(&dir), b"");
    fs.add_denied(layout::lock_path(&dir));
    at.advance(Duration::from_secs(365 * 24 * 60 * 60));

    let due =
        expiry::expired_sessions(Path::new(ROOT), &fs, &at, TTL).unwrap_or_else(|error| panic!("{error}"));

    assert!(due.is_empty(), "{due:?}");
}
