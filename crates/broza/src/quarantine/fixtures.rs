//! Builders shared by the unit tests of the quarantine modules.
//!
//! Everything here is `#[cfg(test)]`: the store's own tests need the same session,
//! the same entries and the same two-device tree over and over, and writing them
//! once keeps the assertions about behaviour rather than about setup.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::clean::{Selection, plan_dry_run};
use crate::model::{
    Category, Finding, FindingPath, ItemStatus, QuarantineEntry, QuarantineSession, SessionId, SessionState,
};
use crate::ports::Answer;
use crate::quarantine::layout;
use crate::quarantine::mover::{MoveRequest, quarantine_items};
use crate::safety::guard::{
    Approved, QuarantineWrite, RestoreRequest, RestoreWrite, Verdict, Write, WriteRequest, approve,
    approve_quarantine_write, approve_restore_targets,
};
use crate::testing::{FakeFileOps, FakePrompter, FixedClock, mac_mount_table};

/// Home directory of the fake user.
pub const HOME: &str = "/Users/dana";
/// Quarantine store of that home.
pub const ROOT: &str = "/Users/dana/.local/share/broza/quarantine";
/// Device of the Data volume, where the store lives.
pub const DATA_DEVICE: u64 = 2;
/// Device of the external volume, which the store cannot reach by rename.
pub const EXTERNAL_DEVICE: u64 = 6;
/// The instant the fixtures call "now".
pub const NOW: &str = "2026-09-21T10:36:08Z";

/// Parse an RFC 3339 instant, panicking with the text that failed.
pub fn at(text: &str) -> Timestamp {
    text.parse().unwrap_or_else(|error| panic!("{text}: {error}"))
}

/// The fixed session identifier every test uses.
pub fn session_id() -> SessionId {
    "cln_20260921103608_a1b2".parse().unwrap_or_else(|error| panic!("{error}"))
}

/// One entry of the fixture session.
pub fn entry(sequence: u32, original: &str, size_bytes: u64, status: ItemStatus) -> QuarantineEntry {
    let id = layout::entry_id(&session_id(), sequence).unwrap_or_else(|error| panic!("{error}"));
    let stored = layout::stored_path(&session_dir(), sequence, layout::basename(original.as_ref()));
    QuarantineEntry {
        id,
        original_path: original.into(),
        stored_path: matches!(status, ItemStatus::Quarantined).then_some(stored),
        restored_to: None,
        size_bytes,
        status,
        error: None,
    }
}

/// A session in `state`, holding `entries`, created at [`NOW`].
pub fn session(state: SessionState, entries: Vec<QuarantineEntry>) -> QuarantineSession {
    let created_at = at(NOW);
    QuarantineSession {
        id: session_id(),
        created_at,
        expires_at: created_at,
        total_bytes: entries.iter().map(|entry| entry.size_bytes).sum(),
        item_count: u64::try_from(entries.len()).unwrap_or(u64::MAX),
        state,
        entries,
    }
}

/// Directory of the fixture session inside the store.
pub fn session_dir() -> PathBuf {
    layout::session_dir(Path::new(ROOT), &session_id())
}

/// Manifest of the fixture session.
pub fn manifest_file() -> PathBuf {
    layout::manifest_path(&session_dir())
}

/// A tree with the Data volume, an external volume and an empty store.
pub fn store_fs() -> FakeFileOps {
    FakeFileOps::new()
        .with_root("/", 1)
        .with_root("/Users", DATA_DEVICE)
        .with_root("/Volumes/External", EXTERNAL_DEVICE)
        .with_dir(ROOT)
}

/// [`store_fs`] with the fixture session directory already created.
pub fn session_fs() -> FakeFileOps {
    store_fs().with_dir(session_dir())
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
    let finding = Finding::builder(id, Category::UserCache, "caches")
        .paths(reported)
        .reclaimable_bytes(paths.iter().map(|(_, size)| *size).fold(0_u64, u64::saturating_add))
        .build()
        .unwrap_or_else(|error| panic!("{error}"));
    vec![finding]
}

/// A token for quarantining `paths`, produced by the real safety kernel.
///
/// Going through [`approve`] rather than forging a token is the point: the unit
/// tests of the store then exercise the same evidence the executor will get,
/// `(device, inode)` included.
pub fn approved_write(fs: &FakeFileOps, paths: &[(&str, u64)]) -> Approved<Write> {
    approved_write_for(fs, paths, Some(PathBuf::from(ROOT)))
}

/// A token whose run never had a quarantine store validated for it.
pub fn approved_without_store(fs: &FakeFileOps, paths: &[(&str, u64)]) -> Approved<Write> {
    approved_write_for(fs, paths, None)
}

/// The shared body of the two: plan, check, confirm.
fn approved_write_for(
    fs: &FakeFileOps,
    paths: &[(&str, u64)],
    quarantine_root: Option<PathBuf>,
) -> Approved<Write> {
    let findings = caches(paths);
    let outcome = plan_dry_run(&findings, &Selection::everything(), session_id(), None)
        .unwrap_or_else(|error| panic!("{error}"));
    let request = WriteRequest { apply: true, tty: true, quarantine_root, ..WriteRequest::new(HOME) };
    match approve(&outcome, &findings, &request, &mac_mount_table(), fs) {
        Ok(Verdict::NeedsConfirmation(pending)) => {
            pending.confirm(&FakePrompter::scripted(&[Answer::Yes])).unwrap_or_else(|error| panic!("{error}"))
        }
        other => panic!("expected a pending approval, got {other:?}"),
    }
}

/// The default retention period, `30d`.
pub const TTL: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 60 * 60);

/// Quarantine `paths` for real, through the guard and the mover.
pub fn quarantined(fs: &FakeFileOps, paths: &[(&str, u64)]) -> QuarantineSession {
    let token = approved_write(fs, paths);
    let request = MoveRequest { ttl: TTL, max_size: None };
    quarantine_items(&token, &request, fs, &FixedClock::at(at(NOW)))
        .unwrap_or_else(|error| panic!("{error}"))
        .session
}

/// The stored paths of a session, plus the session directory itself.
///
/// That is exactly what a `restore`, `expire` or `purge` command approves: the
/// items it will move and the directory it may remove afterwards.
pub fn writable_paths(session: &QuarantineSession) -> Vec<PathBuf> {
    session
        .entries
        .iter()
        .filter_map(|entry| entry.stored_path.clone())
        .chain(std::iter::once(session_dir()))
        .collect()
}

/// A token for writing to `paths` inside the store.
pub fn quarantine_write(fs: &FakeFileOps, paths: &[PathBuf]) -> Approved<QuarantineWrite> {
    approve_quarantine_write(paths, Path::new(ROOT), &mac_mount_table(), fs)
        .unwrap_or_else(|error| panic!("{error}"))
}

/// A token for the places a restore of `session` will write to.
pub fn restore_targets(
    fs: &FakeFileOps,
    session: &QuarantineSession,
    to: Option<&Path>,
) -> Approved<RestoreWrite> {
    let wanted = crate::quarantine::restore::session_destinations(fs, Path::new(ROOT), &session.id, to)
        .unwrap_or_else(|error| panic!("{error}"));
    let request = RestoreRequest {
        to: to.map(Path::to_path_buf),
        quarantine_root: Some(PathBuf::from(ROOT)),
        ..RestoreRequest::new(HOME)
    };
    approve_restore_targets(&wanted, &request, &mac_mount_table(), fs)
        .unwrap_or_else(|error| panic!("{error}"))
}
