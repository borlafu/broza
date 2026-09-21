//! Adversarial probes against the quarantine store.
//!
//! One test per finding of the M3-A review. Each reproduces the situation the
//! finding describes and asserts the behaviour that makes it safe: nothing is
//! deleted that the filesystem still holds, nothing is written where the guard
//! did not approve, and nothing silently overwrites anything.
//!
//! Only the `test-support` feature exposes `broza::testing`; without it this
//! file compiles to nothing.
#![cfg(feature = "test-support")]

mod quarantine_world;

use std::path::{Path, PathBuf};

use broza::BrozaError;
use broza::adapters::StdFileOps;
use broza::model::{ItemStatus, QuarantineEntry, QuarantineSession, SessionId, SessionState};
use broza::ports::{FileOps, already_exists};
use broza::quarantine::mover::MoveRequest;
use broza::quarantine::{codes, expiry, layout, list, manifest, quarantine_items, restore, store};
use broza::safety::guard::{RestoreRequest, approve_restore_targets};
use broza::testing::{FakeFileOps, mac_mount_table};
use quarantine_world::{
    A_DAY, CACHE, HOME, NOW, ROOT, TTL, approved, clock, quarantine, real_mounts, restore_token, store_token,
    tree,
};

/// Rewrite the manifest of `session` with `change` applied to it.
fn rewrite(fs: &FakeFileOps, session: &QuarantineSession, change: impl Fn(&mut QuarantineSession)) {
    let mut edited = session.clone();
    change(&mut edited);
    let path = layout::manifest_path(&layout::session_dir(Path::new(ROOT), &session.id));
    manifest::write(fs, &path, &manifest::Manifest::new(edited)).unwrap_or_else(|error| panic!("{error}"));
}

/// The stored path of the first entry of `session`.
fn first_stored(session: &QuarantineSession) -> PathBuf {
    session.entries[0].stored_path.clone().unwrap_or_else(|| panic!("nothing was moved"))
}

/// CRITICAL 1 — a manifest that lists nothing does not license a deletion.
#[test]
fn a_session_whose_manifest_lost_an_item_is_kept_not_deleted() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let dir = layout::session_dir(Path::new(ROOT), &outcome.session.id);
    let sources = store_token(&fs, &outcome.session);
    let targets = restore_token(&fs, &outcome.session.id, None);
    // The item is on disk; the manifest no longer mentions it.
    rewrite(&fs, &outcome.session, |session| session.entries.clear());

    let reported =
        restore::restore_session(&sources, &targets, &outcome.session.id, Path::new(ROOT), &fs, None)
            .unwrap_or_else(|error| panic!("{error}"));

    assert!(fs.exists(&dir), "the session still holds a file, so it is not removed");
    assert!(fs.exists(&first_stored(&outcome.session)), "and the file is untouched");
    assert_eq!(reported.data.sessions[0].status, ItemStatus::Failed, "never reported as a success");
    assert_eq!(reported.errors[0].code, restore::SESSION_HAS_UNTRACKED_ITEMS);
    assert!(reported.is_partial());
}

/// HIGH 2 — a move interrupted after the rename is adopted on the next read.
#[test]
fn an_item_that_arrived_before_the_crash_is_adopted_as_quarantined() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    // What the manifest looks like between the two writes of one item.
    rewrite(&fs, &outcome.session, |session| {
        session.state = SessionState::InProgress;
        session.entries = vec![QuarantineEntry { status: codes::moving(), ..session.entries[0].clone() }];
    });

    let found =
        store::read_one(&fs, Path::new(ROOT), &outcome.session.id).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(found.session().entries[0].status, ItemStatus::Quarantined);
    assert_eq!(found.session().total_bytes, 12, "it counts again");
    assert!(found.is_accounted_for());
}

/// HIGH 2 — a move interrupted before the rename leaves the item at home.
#[test]
fn an_item_that_never_arrived_is_settled_as_failed() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let stored = first_stored(&outcome.session);
    fs.rename(&stored, Path::new(CACHE)).unwrap_or_else(|error| panic!("{error}"));
    rewrite(&fs, &outcome.session, |session| {
        session.entries = vec![QuarantineEntry { status: codes::moving(), ..session.entries[0].clone() }];
    });

    let found =
        store::read_one(&fs, Path::new(ROOT), &outcome.session.id).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(found.session().entries[0].status, ItemStatus::Failed);
    assert!(found.session().entries[0].stored_path.is_none());
    assert!(fs.exists(Path::new(CACHE)), "the item is still where the user had it");
}

/// HIGH 2 — an item nobody lists keeps the session out of automatic expiry.
#[test]
fn a_session_holding_an_unlisted_item_is_never_expired_automatically() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let dir = layout::session_dir(Path::new(ROOT), &outcome.session.id);
    fs.add_file(dir.join("items/0099/mystery"), b"who put this here");
    at.advance(TTL + A_DAY);
    let due =
        expiry::expired_sessions(Path::new(ROOT), &fs, &at, TTL).unwrap_or_else(|error| panic!("{error}"));

    let reported = expiry::expire(&store_token(&fs, &outcome.session), &due, Path::new(ROOT), &fs)
        .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.sessions[0].status, ItemStatus::Skipped);
    assert_eq!(reported.data.reclaimed_bytes, 0);
    assert!(reported.errors.iter().any(|entry| entry.code == store::ORPHANED_ITEM));
    assert!(fs.exists(&dir.join("items/0099/mystery")), "unattended deletion never happens");
}

/// HIGH 3 — a manifest cannot aim a restore at another user's home.
#[test]
fn a_manifest_that_names_another_users_home_is_refused() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let evil = PathBuf::from("/Users/other-user/Library/LaunchAgents/evil.plist");
    rewrite(&fs, &outcome.session, |session| {
        session.entries[0].original_path = evil.clone();
    });

    let wanted = restore::session_destinations(&fs, Path::new(ROOT), &outcome.session.id, None)
        .unwrap_or_else(|error| panic!("{error}"));
    let refused = approve_restore_targets(&wanted, &RestoreRequest::new(HOME), &mac_mount_table(), &fs);

    assert_eq!(wanted, vec![evil.clone()], "that is where the manifest points");
    assert!(refused.is_err(), "and the guard refuses to approve it: {refused:?}");
    assert!(!fs.exists(&evil));
}

/// HIGH 3 — and the restore itself will not write to an unapproved place.
#[test]
fn a_restore_refuses_a_destination_the_guard_never_saw() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let sources = store_token(&fs, &outcome.session);
    let targets = restore_token(&fs, &outcome.session.id, None);
    rewrite(&fs, &outcome.session, |session| {
        session.entries[0].original_path = PathBuf::from("/Users/dana/Documents/elsewhere");
    });

    let error = restore::restore_session(&sources, &targets, &outcome.session.id, Path::new(ROOT), &fs, None);

    assert!(matches!(error, Err(BrozaError::Other(_))), "{error:?}");
    assert!(!fs.exists(Path::new("/Users/dana/Documents/elsewhere")));
}

/// HIGH 4 — two runs that derive the same identifier get two sessions.
#[test]
fn two_runs_at_the_same_instant_do_not_share_a_session() {
    let fs = tree();
    let at = clock();
    let id = layout::generate_session_id(&at).unwrap_or_else(|error| panic!("{error}"));
    let request = MoveRequest { ttl: TTL, max_size: None };

    let first = quarantine_items(&approved(&fs, &id, &[(CACHE, 12)]), &request, &fs, &at)
        .unwrap_or_else(|error| panic!("{error}"));
    fs.add_file(CACHE, b"a second cache file");
    let second = quarantine_items(&approved(&fs, &id, &[(CACHE, 19)]), &request, &fs, &at)
        .unwrap_or_else(|error| panic!("{error}"));

    assert_ne!(first.session.id, second.session.id, "the second run took the next identifier");
    assert_eq!(second.session.id.timestamp_part(), first.session.id.timestamp_part());
    assert_eq!(*second.plan.session_id(), second.session.id, "the plan reports the one it used");
    let listed =
        list::list_sessions(Path::new(ROOT), &fs, &at, TTL).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(listed.data.sessions.len(), 2);
    assert_eq!(listed.data.total_bytes, 12 + 19, "neither run overwrote the other");
}

/// HIGH 5 — one unreadable session does not hide the store.
#[test]
fn a_corrupt_session_never_hides_the_ones_beside_it() {
    let fs = tree();
    let at = clock();
    let healthy = quarantine(&fs, &at, &[(CACHE, 12)]);
    at.advance(A_DAY);
    fs.add_file(CACHE, b"another cache file");
    let broken = quarantine(&fs, &at, &[(CACHE, 18)]);
    let broken_manifest = layout::manifest_path(&layout::session_dir(Path::new(ROOT), &broken.session.id));
    fs.add_file(&broken_manifest, b"{ truncated");

    let listed =
        list::list_sessions(Path::new(ROOT), &fs, &at, TTL).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(listed.data.sessions.len(), 1, "the healthy session is still listed");
    assert_eq!(listed.data.sessions[0].id, healthy.session.id);
    assert_eq!(listed.errors.len(), 1);
    assert_eq!(listed.errors[0].code, store::MANIFEST_CORRUPT);
    assert!(listed.is_partial(), "exit 5, not exit 1");
}

/// HIGH 5 — `purge --all` skips the unreadable one and removes the rest.
#[test]
fn purging_everything_skips_what_it_cannot_read() {
    let fs = tree();
    let at = clock();
    let healthy = quarantine(&fs, &at, &[(CACHE, 12)]);
    let broken: SessionId = "cln_20260801091200_c3d4".parse().unwrap_or_else(|e| panic!("{e}"));
    let broken_dir = layout::session_dir(Path::new(ROOT), &broken);
    fs.add_file(broken_dir.join("manifest.json"), b"{ truncated");
    let ids = vec![healthy.session.id.clone(), broken];

    let reported = expiry::purge(&store_token(&fs, &healthy.session), &ids, Path::new(ROOT), &fs)
        .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.sessions[0].status, ItemStatus::Purged);
    assert_eq!(reported.data.sessions[1].status, ItemStatus::Skipped);
    assert_eq!(reported.errors[0].code, store::MANIFEST_CORRUPT);
    assert!(fs.exists(&broken_dir), "what cannot be read is not deleted blind");
}

/// MEDIUM 6 — a restore that could not finish stays retryable.
#[test]
fn a_partly_restored_session_stays_in_the_restoring_state() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let sources = store_token(&fs, &outcome.session);
    let targets = restore_token(&fs, &outcome.session.id, None);
    fs.add_file(CACHE, b"something took the name back");

    let reported =
        restore::restore_session(&sources, &targets, &outcome.session.id, Path::new(ROOT), &fs, None)
            .unwrap_or_else(|error| panic!("{error}"));

    let left =
        store::read_one(&fs, Path::new(ROOT), &outcome.session.id).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(reported.data.sessions[0].status, ItemStatus::Skipped);
    assert_eq!(left.session().state, SessionState::Restoring, "so the command can be run again");
    assert_eq!(left.session().entries[0].status, ItemStatus::Quarantined, "the item is still ours");
    assert!(reported.errors[0].message.contains("broza restore --session"), "{:?}", reported.errors[0]);
}

/// 7 — a rename inside the store never replaces what is already there.
#[test]
fn an_exclusive_rename_refuses_to_overwrite_on_both_filesystems() {
    let fake = tree();
    fake.add_file("/Users/dana/from", b"new");
    fake.add_file("/Users/dana/to", b"old");

    let refused = fake.rename_exclusive(Path::new("/Users/dana/from"), Path::new("/Users/dana/to"));

    assert!(refused.is_err_and(|error| already_exists(&error)), "the fake reports EEXIST");
    assert_eq!(fake.read(Path::new("/Users/dana/to")).ok(), Some(b"old".to_vec()));

    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let (from, to) = (temp.path().join("from"), temp.path().join("to"));
    StdFileOps.write_atomic(&from, b"new").unwrap_or_else(|error| panic!("{error}"));
    StdFileOps.write_atomic(&to, b"old").unwrap_or_else(|error| panic!("{error}"));

    let refused = StdFileOps.rename_exclusive(&from, &to);

    assert!(refused.is_err_and(|error| already_exists(&error)), "and so does the real filesystem");
    assert_eq!(StdFileOps.read(&to).ok(), Some(b"old".to_vec()));
    assert!(StdFileOps.exists(&from), "the source is still there to retry with");
}

/// 7 — and an exclusive directory creation is how a session name is claimed.
#[test]
fn an_exclusive_directory_creation_refuses_a_name_that_is_taken() {
    let fake = tree();
    fake.add_dir("/Users/dana/taken");

    let refused = fake.create_dir_exclusive(Path::new("/Users/dana/taken"));

    assert!(refused.is_err_and(|error| already_exists(&error)));

    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let taken = temp.path().join("taken");
    StdFileOps.create_dir_exclusive(&taken).unwrap_or_else(|error| panic!("{error}"));

    let refused = StdFileOps.create_dir_exclusive(&taken);

    assert!(refused.is_err_and(|error| already_exists(&error)));
}

/// 8 — bytes the manifest does not know about are still reported as held.
#[test]
fn the_listing_counts_what_the_manifest_forgot() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let dir = layout::session_dir(Path::new(ROOT), &outcome.session.id);
    fs.add_file(dir.join("items/0099/mystery"), b"unaccounted for");

    let listed =
        list::list_sessions(Path::new(ROOT), &fs, &at, TTL).unwrap_or_else(|error| panic!("{error}"));

    assert!(listed.data.total_bytes > 12, "the orphan occupies the disk too");
    assert!(listed.warnings.iter().any(|warning| warning.code == list::UNTRACKED_BYTES), "{listed:?}");
    assert!(!listed.is_partial(), "a warning, not an error: nothing failed");
}

/// 9 — a session an interrupted move left behind is reclaimable.
#[test]
fn a_session_left_in_progress_expires_once_it_is_settled() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    rewrite(&fs, &outcome.session, |session| session.state = SessionState::InProgress);
    at.advance(TTL + A_DAY);

    let due =
        expiry::expired_sessions(Path::new(ROOT), &fs, &at, TTL).unwrap_or_else(|error| panic!("{error}"));
    let reported = expiry::expire(&store_token(&fs, &outcome.session), &due, Path::new(ROOT), &fs)
        .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(due, vec![outcome.session.id.clone()]);
    assert_eq!(reported.data.reclaimed_bytes, 12);
    assert!(!fs.exists(&layout::session_dir(Path::new(ROOT), &outcome.session.id)));
}

/// 10 — the mover writes where the guard said, not where the caller says.
#[test]
fn the_store_the_mover_uses_is_the_one_the_guard_validated() {
    let fs = tree();
    let at = clock();
    let id = layout::generate_session_id(&at).unwrap_or_else(|error| panic!("{error}"));
    let token = approved(&fs, &id, &[(CACHE, 12)]);

    assert_eq!(token.quarantine_root(), Some(Path::new(ROOT)));

    let outcome = quarantine_items(&token, &MoveRequest { ttl: TTL, max_size: None }, &fs, &at)
        .unwrap_or_else(|error| panic!("{error}"));

    let stored = first_stored(&outcome.session);
    assert!(stored.starts_with(ROOT), "{}", stored.display());
}

/// 11 — the cap is applied to the blocks a directory really occupies.
#[test]
fn the_cap_is_measured_in_allocated_blocks() {
    let fs = tree();
    let at = clock();
    let id = layout::generate_session_id(&at).unwrap_or_else(|error| panic!("{error}"));
    // Two tiny files: 10 apparent bytes, but two whole allocation units on disk.
    let token = approved(&fs, &id, &[(quarantine_world::TREE, 10)]);
    let capped = MoveRequest { ttl: TTL, max_size: Some(4_096) };

    let outcome = quarantine_items(&token, &capped, &fs, &at).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(outcome.session.entries[0].status, ItemStatus::Skipped);
    assert_eq!(outcome.session.entries[0].error, Some(codes::max_size_exceeded()));
    assert!(fs.exists(Path::new(quarantine_world::TREE)), "an item over the cap is left alone");
}

/// A real filesystem claims a session name exactly the way the fake does.
#[test]
fn a_real_store_claims_its_session_name_exclusively() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let base = std::fs::canonicalize(temp.path()).unwrap_or_else(|error| panic!("{error}"));
    let root = base.join("quarantine");
    let id: SessionId = format!("cln_{}_0000", NOW.replace(['-', ':', 'T', 'Z'], ""))
        .replace("103608", "103608")
        .parse()
        .unwrap_or_else(|error| panic!("{error}"));
    StdFileOps.create_dir_all(&root).unwrap_or_else(|error| panic!("{error}"));
    let dir = layout::session_dir(&root, &id);

    StdFileOps.create_dir_exclusive(&dir).unwrap_or_else(|error| panic!("{error}"));
    let again = StdFileOps.create_dir_exclusive(&dir);
    let next = layout::session_dir(&root, &layout::next_session_id(&id).unwrap_or_else(|e| panic!("{e}")));

    assert!(again.is_err_and(|error| already_exists(&error)));
    assert!(StdFileOps.create_dir_exclusive(&next).is_ok(), "the next name is free");
    let _ = real_mounts(0);
}
