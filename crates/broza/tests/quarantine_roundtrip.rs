//! The quarantine store from outside the crate: move, list, restore, expire, purge.
//!
//! Every token comes from the real safety kernel — `approve` over
//! [`mac_mount_table`](broza::testing::mac_mount_table) with a scripted
//! prompter — so these tests exercise the same path `broza-cli` will: plan,
//! approve, confirm, move, and only then touch the store.
//!
//! Only the `test-support` feature exposes `broza::testing`; without it this
//! file compiles to nothing.
#![cfg(feature = "test-support")]

mod quarantine_world;

use std::path::Path;

use broza::BrozaError;
use broza::adapters::StdFileOps;
use broza::model::{ItemErrorCode, ItemStatus, QuarantineEntry, QuarantineSession, SessionState};
use broza::ports::{Clock, FileOps};
use broza::quarantine::manifest::{self, Manifest};
use broza::quarantine::mover::{MoveOutcome, MoveRequest};
use broza::quarantine::{expiry, layout, list, quarantine_items, restore, store};
use broza::safety::guard::{RestoreRequest, approve_quarantine_write, approve_restore_targets};
use quarantine_world::{
    A_DAY, CACHE, CACHES, EXTERNAL, HOME, ROOT, TREE, TTL, approved, clock, quarantine, real_mounts,
    restore_token, snapshot, store_token, tree,
};

/// The status of the entry for `original`, or a panic naming what was there.
fn status_of(outcome: &MoveOutcome, original: &str) -> (ItemStatus, Option<ItemErrorCode>) {
    let entry = outcome
        .session
        .entries
        .iter()
        .find(|entry| entry.original_path == Path::new(original))
        .unwrap_or_else(|| panic!("no entry for {original}"));
    (entry.status.clone(), entry.error.clone())
}

#[test]
fn a_cleanup_quarantines_what_it_can_and_explains_the_rest() {
    let fs = tree();
    let at = clock();
    let id = layout::generate_session_id(&at).unwrap_or_else(|error| panic!("{error}"));
    let paths = [(CACHE, 12), (TREE, 11), (EXTERNAL, 15)];
    let token = approved(&fs, &id, &paths);
    // The directory is replaced between the check and the move: a new inode.
    fs.remove_tree(Path::new(TREE)).unwrap_or_else(|error| panic!("{error}"));
    fs.add_file(format!("{TREE}/a"), b"an impostor");

    let request = MoveRequest { ttl: TTL, max_size: None };
    let outcome = quarantine_items(&token, &request, &fs, &at).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(status_of(&outcome, CACHE), (ItemStatus::Quarantined, None));
    assert_eq!(status_of(&outcome, TREE).0, ItemStatus::Failed);
    assert_eq!(status_of(&outcome, EXTERNAL), (ItemStatus::Skipped, Some(ItemErrorCode::CrossVolume)));
    assert_eq!(outcome.session.state, SessionState::Complete);
    assert_eq!(outcome.plan.quarantined_bytes(), 12, "only the cache file moved");
    assert_eq!(outcome.plan.reclaimed_bytes(), 0, "quarantining frees nothing");
    assert!(fs.exists(Path::new(EXTERNAL)) && fs.exists(Path::new(TREE)));
    assert!(!fs.exists(Path::new(CACHE)));
}

#[test]
fn a_cleanup_is_listed_as_one_session_holding_the_bytes_it_moved() {
    let fs = tree();
    let at = clock();
    quarantine(&fs, &at, &[(CACHE, 12)]);

    let listed =
        list::list_sessions(Path::new(ROOT), &fs, &at, TTL).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(listed.data.sessions.len(), 1);
    assert_eq!(listed.data.total_bytes, 12);
    assert_eq!(listed.data.expired_bytes, 0);
    assert_eq!(listed.data.sessions[0].state, SessionState::Complete);
    assert!(listed.warnings.is_empty());
}

#[test]
fn restoring_a_session_rebuilds_the_tree_byte_for_byte() {
    let fs = tree();
    let at = clock();
    let before = snapshot(&fs, CACHES);

    let outcome = quarantine(&fs, &at, &[(CACHE, 12), (TREE, 11)]);
    assert_ne!(snapshot(&fs, CACHES), before, "the items really left");

    let reported = restore::restore_session(
        &store_token(&fs, &outcome.session),
        &restore_token(&fs, &outcome.session.id, None),
        &outcome.session.id,
        Path::new(ROOT),
        &fs,
        None,
    )
    .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.sessions[0].status, ItemStatus::Restored);
    assert_eq!(reported.data.restored_bytes, 12 + 11);
    assert!(!reported.is_partial());
    assert_eq!(snapshot(&fs, CACHES), before, "every file is back, with its bytes");
    assert!(!fs.exists(&layout::session_dir(Path::new(ROOT), &outcome.session.id)));
}

#[test]
fn a_session_is_reclaimed_once_its_retention_period_is_over() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let token = store_token(&fs, &outcome.session);

    assert_eq!(expiry::expired_sessions(Path::new(ROOT), &fs, &at, TTL).ok(), Some(Vec::new()));
    at.advance(TTL + A_DAY);
    let due =
        expiry::expired_sessions(Path::new(ROOT), &fs, &at, TTL).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(due, vec![outcome.session.id.clone()]);

    let reported =
        expiry::expire(&token, &due, Path::new(ROOT), &fs).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.reclaimed_bytes, 12, "expiry is where the space is actually freed");
    assert_eq!(reported.data.sessions[0].status, ItemStatus::Purged);
    assert!(!fs.exists(&layout::session_dir(Path::new(ROOT), &outcome.session.id)));
    assert!(fs.exists(Path::new(ROOT)));
}

#[test]
fn purging_removes_a_session_that_is_nowhere_near_its_expiry() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let token = store_token(&fs, &outcome.session);

    let reported = expiry::purge(&token, std::slice::from_ref(&outcome.session.id), Path::new(ROOT), &fs)
        .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.reclaimed_bytes, 12);
    assert!(!fs.exists(Path::new(CACHE)), "a purge is irreversible");
    assert_eq!(
        list::list_sessions(Path::new(ROOT), &fs, &at, TTL).map(|listed| listed.data.sessions.len()).ok(),
        Some(0)
    );
}

#[test]
fn a_corrupt_manifest_is_an_error_and_never_a_panic() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let path = layout::manifest_path(&layout::session_dir(Path::new(ROOT), &outcome.session.id));
    fs.add_file(&path, b"{ \"manifest_version\": 1, \"session\": ");

    let listed =
        list::list_sessions(Path::new(ROOT), &fs, &at, TTL).unwrap_or_else(|error| panic!("{error}"));
    let read = manifest::read(&fs, &path);

    assert!(matches!(read, Err(BrozaError::Other(_))), "{read:?}");
    assert!(listed.data.sessions.is_empty(), "the unreadable session is not listed");
    assert_eq!(listed.errors.len(), 1, "but it is reported");
    assert_eq!(listed.errors[0].code, store::MANIFEST_CORRUPT);
    assert!(listed.is_partial(), "which means exit 5, not a crash");
}

#[test]
fn a_manifest_from_a_newer_broza_keeps_its_unknown_state() {
    let fs = tree();
    let at = clock();
    let outcome = quarantine(&fs, &at, &[(CACHE, 12)]);
    let path = layout::manifest_path(&layout::session_dir(Path::new(ROOT), &outcome.session.id));
    let raw = String::from_utf8(fs.read(&path).unwrap_or_default()).unwrap_or_default();
    fs.add_file(&path, raw.replace("\"complete\"", "\"archived\"").as_bytes());

    let found =
        store::read_one(&fs, Path::new(ROOT), &outcome.session.id).unwrap_or_else(|error| panic!("{error}"));
    manifest::write(&fs, &path, &found.manifest).unwrap_or_else(|error| panic!("{error}"));
    let again = String::from_utf8(fs.read(&path).unwrap_or_default()).unwrap_or_default();

    assert_eq!(found.session().state, SessionState::Unknown("archived".to_owned()));
    assert!(again.contains("\"archived\""), "an unknown token survives verbatim: {again}");
    let listed =
        list::list_sessions(Path::new(ROOT), &fs, &at, TTL).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(listed.data.sessions[0].state, SessionState::Unknown("archived".to_owned()));
}

#[test]
fn a_session_is_restored_from_the_real_filesystem() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let base = std::fs::canonicalize(temp.path()).unwrap_or_else(|error| panic!("{error}"));
    let root = base.join("quarantine");
    let source = base.join("work/app.cache");
    let at = clock();
    let id = layout::generate_session_id(&at).unwrap_or_else(|error| panic!("{error}"));
    let session_dir = layout::session_dir(&root, &id);
    let stored = layout::stored_path(&session_dir, 1, layout::basename(&source));
    for directory in [&root, &layout::item_dir(&session_dir, 1)] {
        StdFileOps.create_dir_all(directory).unwrap_or_else(|error| panic!("{error}"));
    }
    StdFileOps.create_dir_all(&base.join("work")).unwrap_or_else(|error| panic!("{error}"));
    StdFileOps.write_atomic(&source, b"real bytes").unwrap_or_else(|error| panic!("{error}"));
    StdFileOps.rename(&source, &stored).unwrap_or_else(|error| panic!("{error}"));

    let entry = QuarantineEntry {
        id: layout::entry_id(&id, 1).unwrap_or_else(|error| panic!("{error}")),
        original_path: source.clone(),
        stored_path: Some(stored.clone()),
        restored_to: None,
        size_bytes: 10,
        status: ItemStatus::Quarantined,
        error: None,
    };
    let session = QuarantineSession {
        id: id.clone(),
        created_at: at.now(),
        expires_at: at.now(),
        total_bytes: 10,
        item_count: 1,
        state: SessionState::Complete,
        entries: vec![entry],
    };
    let manifest_path = layout::manifest_path(&session_dir);
    manifest::write(&StdFileOps, &manifest_path, &Manifest::new(session))
        .unwrap_or_else(|error| panic!("{error}"));

    let device = StdFileOps.metadata(&root).unwrap_or_else(|error| panic!("{error}")).device;
    let mounts = real_mounts(device);
    let sources =
        approve_quarantine_write(&[stored.clone(), session_dir.clone()], &root, &mounts, &StdFileOps)
            .unwrap_or_else(|error| panic!("{error}"));
    // A real temporary directory is not under `$HOME`, so the destinations are
    // approved the way `broza restore --to` approves them.
    let rescued = base.join("rescued");
    let wanted = restore::session_destinations(&StdFileOps, &root, &id, Some(&rescued))
        .unwrap_or_else(|error| panic!("{error}"));
    let request = RestoreRequest { to: Some(rescued.clone()), ..RestoreRequest::new(HOME) };
    let targets = approve_restore_targets(&wanted, &request, &mounts, &StdFileOps)
        .unwrap_or_else(|error| panic!("{error}"));

    let reported = restore::restore_session(&sources, &targets, &id, &root, &StdFileOps, Some(&rescued))
        .unwrap_or_else(|error| panic!("{error}"));

    let back = rescued.join("0001_app.cache");
    assert_eq!(reported.data.restored_bytes, 10);
    assert_eq!(StdFileOps.read(&back).ok(), Some(b"real bytes".to_vec()));
    assert!(!StdFileOps.exists(&stored));
    assert!(!StdFileOps.exists(&session_dir), "an emptied session is deleted for real too");
}

#[test]
fn a_manifest_is_written_before_anything_moves() {
    let fs = tree();
    let at = clock();
    let id = layout::generate_session_id(&at).unwrap_or_else(|error| panic!("{error}"));
    let token = approved(&fs, &id, &[(CACHE, 12)]);
    let request = MoveRequest { ttl: TTL, max_size: Some(0) };

    let outcome = quarantine_items(&token, &request, &fs, &at).unwrap_or_else(|error| panic!("{error}"));

    let found = store::read_one(&fs, Path::new(ROOT), &id).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(outcome.plan.quarantined_bytes(), 0, "the cap left no room");
    assert_eq!(found.session().entries.len(), 1, "the manifest still records the attempt");
    assert_eq!(found.session().entries[0].status, ItemStatus::Skipped);
    assert!(fs.exists(Path::new(CACHE)));
}
