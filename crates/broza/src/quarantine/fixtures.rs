//! Builders shared by the unit tests of the quarantine modules.
//!
//! Everything here is `#[cfg(test)]`: the store's own tests need the same session,
//! the same entries and the same two-device tree over and over, and writing them
//! once keeps the assertions about behaviour rather than about setup.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::model::{
    ItemStatus, QuarantineEntry, QuarantineSession, SessionId, SessionState,
};
use crate::quarantine::layout;
use crate::testing::FakeFileOps;

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
