//! The world the quarantine integration tests share.
//!
//! Built on the crate's own fakes (`broza::testing`, feature `test-support`) and
//! on [`mac_mount_table`], so the volumes, roles and devices are the ones a real
//! Apple Silicon Mac reports. Every token comes from the real safety kernel:
//! plan, approve, confirm, and only then touch the store.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use broza::clean::{Selection, plan_dry_run};
use broza::model::{Category, Finding, FindingPath, QuarantineSession, SessionId, Volume, VolumeRole};
use broza::ports::{Answer, FileOps};
use broza::quarantine::mover::{MoveOutcome, MoveRequest};
use broza::quarantine::{layout, quarantine_items, restore};
use broza::safety::guard::{
    Approved, QuarantineWrite, RestoreRequest, RestoreWrite, Verdict, Write, WriteRequest, approve,
    approve_quarantine_write, approve_restore_targets,
};
use broza::testing::{FakeFileOps, FakePrompter, FixedClock, mac_mount_table};

/// Home of the fake user.
pub const HOME: &str = "/Users/dana";
/// Quarantine store of that home.
pub const ROOT: &str = "/Users/dana/.local/share/broza/quarantine";
/// A cache file that can be moved.
pub const CACHE: &str = "/Users/dana/Library/Caches/app.cache";
/// A cache directory with contents, to prove a whole tree survives.
pub const TREE: &str = "/Users/dana/Library/Caches/DerivedData";
/// A path on the external volume: another device, so it cannot be renamed.
pub const EXTERNAL: &str = "/Volumes/External/.Trashes/501/old.dmg";
/// Where the caches live, the subtree the round-trip test compares.
pub const CACHES: &str = "/Users/dana/Library/Caches";
/// The instant the fixtures call "now".
pub const NOW: &str = "2026-09-21T10:36:08Z";
/// The default retention period.
pub const TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// A day, for moving the clock.
pub const A_DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// The tree every test starts from: a Data volume, an external disk, a store.
pub fn tree() -> FakeFileOps {
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

/// A clock frozen at [`NOW`].
pub fn clock() -> FixedClock {
    FixedClock::at(NOW.parse().unwrap_or_else(|error| panic!("{error}")))
}

/// One green `user-cache` finding covering `paths`.
pub fn caches(paths: &[(&str, u64)]) -> Vec<Finding> {
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
pub fn approved(fs: &FakeFileOps, id: &SessionId, paths: &[(&str, u64)]) -> Approved<Write> {
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

/// Move `paths` into a new session of the store, at the clock's instant.
pub fn quarantine(fs: &FakeFileOps, at: &FixedClock, paths: &[(&str, u64)]) -> MoveOutcome {
    let id = layout::generate_session_id(at).unwrap_or_else(|error| panic!("{error}"));
    let token = approved(fs, &id, paths);
    let request = MoveRequest { ttl: TTL, max_size: None };
    quarantine_items(&token, &request, fs, at).unwrap_or_else(|error| panic!("{error}"))
}

/// A token for moving a session's items out of the store, or removing it.
pub fn store_token(fs: &FakeFileOps, session: &QuarantineSession) -> Approved<QuarantineWrite> {
    let paths: Vec<PathBuf> = session
        .entries
        .iter()
        .filter_map(|entry| entry.stored_path.clone())
        .chain(std::iter::once(layout::session_dir(Path::new(ROOT), &session.id)))
        .collect();
    approve_quarantine_write(&paths, Path::new(ROOT), &mac_mount_table(), fs)
        .unwrap_or_else(|error| panic!("{error}"))
}

/// A token for the places restoring `session` will write to.
pub fn restore_token(fs: &FakeFileOps, session: &SessionId, to: Option<&Path>) -> Approved<RestoreWrite> {
    let wanted = restore::session_destinations(fs, Path::new(ROOT), session, to)
        .unwrap_or_else(|error| panic!("{error}"));
    let request = RestoreRequest {
        to: to.map(Path::to_path_buf),
        quarantine_root: Some(PathBuf::from(ROOT)),
        ..RestoreRequest::new(HOME)
    };
    approve_restore_targets(&wanted, &request, &mac_mount_table(), fs)
        .unwrap_or_else(|error| panic!("{error}"))
}

/// Every path below `root`, with the bytes of each file.
pub fn snapshot(fs: &FakeFileOps, root: &str) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    fs.paths()
        .into_iter()
        .filter(|path| path.starts_with(root))
        .map(|path| {
            let contents = fs.read(&path).ok();
            (path, contents)
        })
        .collect()
}

/// A mount table describing one writable volume at `/`, on a real device.
pub fn real_mounts(device: u64) -> broza::scan::MountTable {
    broza::scan::MountTable::new(vec![broza::scan::MountEntry {
        mount_point: PathBuf::from("/"),
        device,
        volume: Volume {
            id: "disk3s5".parse().unwrap_or_else(|error| panic!("{error}")),
            name: "Test".to_owned(),
            uuid: None,
            role: VolumeRole::Data,
            mount_point: Some(PathBuf::from("/")),
            used_bytes: 0,
            writable_by_broza: true,
            purpose: String::new(),
        },
        firmlinks: Vec::new(),
    }])
}
