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

use std::path::{Path, PathBuf};
use std::time::Duration;

use broza::BrozaError;
use broza::adapters::StdFileOps;
use broza::clean::{Selection, plan_dry_run};
use broza::model::{
    Category, Finding, FindingPath, ItemErrorCode, ItemStatus, QuarantineEntry, QuarantineSession, SessionId,
    SessionState, Volume, VolumeRole,
};
use broza::ports::{Answer, Clock, FileOps};
use broza::quarantine::manifest::{self, Manifest};
use broza::quarantine::mover::{MoveOutcome, MoveRequest};
use broza::quarantine::{expiry, layout, list, quarantine_items, restore, store};
use broza::safety::guard::{
    Approved, QuarantineWrite, Verdict, Write, WriteRequest, approve, approve_quarantine_write,
};
use broza::scan::{MountEntry, MountTable};
use broza::testing::{FakeFileOps, FakePrompter, FixedClock, mac_mount_table};

/// Home of the fake user.
const HOME: &str = "/Users/dana";
/// Quarantine store of that home.
const ROOT: &str = "/Users/dana/.local/share/broza/quarantine";
/// A cache file that can be moved.
const CACHE: &str = "/Users/dana/Library/Caches/app.cache";
/// A cache directory with contents, to prove a whole tree survives.
const TREE: &str = "/Users/dana/Library/Caches/DerivedData";
/// A path on the external volume: another device, so it cannot be renamed.
const EXTERNAL: &str = "/Volumes/External/.Trashes/501/old.dmg";
/// Where the caches live, the subtree the round-trip test compares.
const CACHES: &str = "/Users/dana/Library/Caches";
/// The default retention period.
const TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// A day, for moving the clock.
const A_DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// The tree every test starts from: a Data volume, an external disk, a store.
fn tree() -> FakeFileOps {
    FakeFileOps::new()
        .with_root("/", 1)
        .with_root("/Users", 2)
        .with_root("/Volumes/External", 6)
        .with_dir(ROOT)
        .with_file(CACHE, b"a cache file")
        .with_file(format!("{TREE}/a"), b"first")
        .with_file(format!("{TREE}/deep/b"), b"second")
        .with_file(EXTERNAL, b"on another disk")
}

/// A clock frozen at the instant the fixtures call "now".
fn clock() -> FixedClock {
    FixedClock::at("2026-09-21T10:36:08Z".parse().unwrap_or_else(|error| panic!("{error}")))
}

/// One green `user-cache` finding covering `paths`.
fn caches(paths: &[(&str, u64)]) -> Vec<Finding> {
    let reported = paths
        .iter()
        .map(|(path, size_bytes)| FindingPath {
            path: PathBuf::from(*path),
            size_bytes: *size_bytes,
            last_used: None,
        })
        .collect();
    let id = "user-cache.app".parse().unwrap_or_else(|error| panic!("{error}"));
    vec![
        Finding::builder(id, Category::UserCache, "caches")
            .paths(reported)
            .reclaimable_bytes(paths.iter().map(|(_, size)| *size).fold(0_u64, u64::saturating_add))
            .build()
            .unwrap_or_else(|error| panic!("{error}")),
    ]
}

/// Plan, check and confirm a cleanup of `paths`, exactly as the CLI will.
fn approved(fs: &FakeFileOps, id: &SessionId, paths: &[(&str, u64)]) -> Approved<Write> {
    let findings = caches(paths);
    let outcome = plan_dry_run(&findings, &Selection::everything(), id.clone(), None)
        .unwrap_or_else(|error| panic!("{error}"));
    let request = WriteRequest {
        apply: true,
        tty: true,
        quarantine_root: Some(PathBuf::from(ROOT)),
        ..WriteRequest::new(HOME)
    };
    match approve(&outcome, &findings, &request, &mac_mount_table(), fs) {
        Ok(Verdict::NeedsConfirmation(pending)) => {
            pending.confirm(&FakePrompter::scripted(&[Answer::Yes])).unwrap_or_else(|error| panic!("{error}"))
        }
        other => panic!("expected a pending approval, got {other:?}"),
    }
}

/// Move `paths` into a new session of the store.
fn quarantine(fs: &FakeFileOps, at: &FixedClock, paths: &[(&str, u64)]) -> MoveOutcome {
    let id = layout::generate_session_id(at).unwrap_or_else(|error| panic!("{error}"));
    let token = approved(fs, &id, paths);
    let request = MoveRequest { root: PathBuf::from(ROOT), ttl: TTL, max_size: None };
    quarantine_items(&token, &request, fs, at).unwrap_or_else(|error| panic!("{error}"))
}

/// A token for putting a session back or removing it.
fn store_token(fs: &FakeFileOps, session: &QuarantineSession) -> Approved<QuarantineWrite> {
    let paths: Vec<PathBuf> = session
        .entries
        .iter()
        .filter_map(|entry| entry.stored_path.clone())
        .chain(std::iter::once(layout::session_dir(Path::new(ROOT), &session.id)))
        .collect();
    approve_quarantine_write(&paths, Path::new(ROOT), &mac_mount_table(), fs)
        .unwrap_or_else(|error| panic!("{error}"))
}

/// Every path below `root`, with the bytes of each file.
fn snapshot(fs: &FakeFileOps, root: &str) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    fs.paths()
        .into_iter()
        .filter(|path| path.starts_with(root))
        .map(|path| {
            let contents = fs.read(&path).ok();
            (path, contents)
        })
        .collect()
}

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

    let request = MoveRequest { root: PathBuf::from(ROOT), ttl: TTL, max_size: None };
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

    let listed = list::list_sessions(Path::new(ROOT), &fs, &at, TTL);
    let read = manifest::read(&fs, &path);

    assert!(matches!(listed, Err(BrozaError::Other(_))), "{listed:?}");
    assert!(matches!(read, Err(BrozaError::Other(_))), "{read:?}");
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

/// A mount table describing one writable volume at `/`, on the real device.
fn real_mounts(device: u64) -> MountTable {
    MountTable::new(vec![MountEntry {
        mount_point: PathBuf::from("/"),
        device,
        volume: Volume {
            id: "disk3s5".parse().unwrap_or_else(|error| panic!("{error}")),
            name: "Test".to_owned(),
            role: VolumeRole::Data,
            mount_point: Some(PathBuf::from("/")),
            used_bytes: 0,
            writable_by_broza: true,
            purpose: String::new(),
        },
        firmlinks: Vec::new(),
    }])
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
    let token = approve_quarantine_write(
        &[stored.clone(), session_dir.clone()],
        &root,
        &real_mounts(device),
        &StdFileOps,
    )
    .unwrap_or_else(|error| panic!("{error}"));

    let reported = restore::restore_session(&token, &id, &root, &StdFileOps, None)
        .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(reported.data.restored_bytes, 10);
    assert_eq!(StdFileOps.read(&source).ok(), Some(b"real bytes".to_vec()));
    assert!(!StdFileOps.exists(&stored));
    assert!(!StdFileOps.exists(&session_dir), "an emptied session is deleted for real too");
}

#[test]
fn a_manifest_is_written_before_anything_moves() {
    let fs = tree();
    let at = clock();
    let id = layout::generate_session_id(&at).unwrap_or_else(|error| panic!("{error}"));
    let token = approved(&fs, &id, &[(CACHE, 12)]);
    let request = MoveRequest { root: PathBuf::from(ROOT), ttl: TTL, max_size: Some(0) };

    let outcome = quarantine_items(&token, &request, &fs, &at).unwrap_or_else(|error| panic!("{error}"));

    let found = store::read_one(&fs, Path::new(ROOT), &id).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(outcome.plan.quarantined_bytes(), 0, "the cap left no room");
    assert_eq!(found.session().entries.len(), 1, "the manifest still records the attempt");
    assert_eq!(found.session().entries[0].status, ItemStatus::Skipped);
    assert!(fs.exists(Path::new(CACHE)));
}
