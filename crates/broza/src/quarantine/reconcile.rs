//! Making a manifest agree with what is actually under `items/`.
//!
//! The mover writes an entry *before* it renames and flips it to `quarantined`
//! after, so a run killed mid-move leaves a `moving` entry. Reading a session
//! settles those against the filesystem instead of trusting either side:
//!
//! | on disk | in the manifest | reconciled to |
//! |---|---|---|
//! | present | `moving` | `quarantined` — the rename did happen |
//! | absent | `moving` | `failed` — the rename did not, the item is still home |
//! | present | nothing claims it | an **orphan**: reported, never silently dropped |
//!
//! An orphan is data Broza moved and then lost track of. It is never removed by
//! an automatic step; only an explicit `purge` may.

use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::model::{ItemErrorCode, ItemStatus, QuarantineEntry, QuarantineSession};
use crate::ports::FileOps;
use crate::quarantine::codes::moving;
use crate::quarantine::entries::with_entries;
use crate::quarantine::layout::{ITEMS_DIR, LOCK_FILE, MANIFEST_FILE};

/// What a session looks like once the manifest and the disk agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciled {
    /// The session, with every `moving` entry settled.
    pub session: QuarantineSession,
    /// Stored items no entry claims, sorted.
    pub orphans: Vec<PathBuf>,
}

/// Settle `session` against the items really present under `dir`.
///
/// # Errors
///
/// Whatever reading `items/` reports, except a missing `items/` directory: a
/// session that never moved anything simply holds nothing.
pub fn reconcile(
    session: &QuarantineSession,
    dir: &Path,
    fs: &dyn FileOps,
) -> Result<Reconciled, BrozaError> {
    let on_disk = stored_items(fs, dir)?;
    let entries: Vec<QuarantineEntry> = session.entries.iter().map(|entry| settle(entry, &on_disk)).collect();
    let orphans = on_disk
        .into_iter()
        .filter(|stored| !entries.iter().any(|entry| entry.stored_path.as_ref() == Some(stored)))
        .collect();
    Ok(Reconciled { session: with_entries(session, entries), orphans })
}

/// Every `items/<seq>/<name>` that exists under `dir`, sorted.
///
/// Only that one depth is a stored item. The `items/<seq>` directories
/// themselves are bookkeeping and an empty one means the item left.
///
/// # Errors
///
/// Whatever [`FileOps::read_dir`] reports for a directory that is there.
pub fn stored_items(fs: &dyn FileOps, dir: &Path) -> Result<Vec<PathBuf>, BrozaError> {
    let items = dir.join(ITEMS_DIR);
    if !fs.exists(&items) {
        return Ok(Vec::new());
    }
    let mut stored = Vec::new();
    for sequence in fs.read_dir(&items)? {
        if !fs.metadata(&sequence)?.is_dir {
            // Not a sequence directory, so nothing put it there on purpose.
            stored.push(sequence);
            continue;
        }
        stored.extend(fs.read_dir(&sequence)?);
    }
    stored.sort();
    Ok(stored)
}

/// Everything in the session directory that is not the session's own plumbing.
///
/// `manifest.json`, the `.lock` file and an `items/` tree are what a session
/// *is*; the stored items inside `items/` and anything else at all are what it
/// *holds*. An empty result is the only thing that licenses removing the
/// directory (`docs/cli-spec.md` §3.5).
///
/// # Errors
///
/// Whatever reading the session directory reports.
pub fn leftovers(fs: &dyn FileOps, dir: &Path) -> Result<Vec<PathBuf>, BrozaError> {
    let mut left = stored_items(fs, dir)?;
    let plumbing = [ITEMS_DIR, MANIFEST_FILE, LOCK_FILE];
    for child in fs.read_dir(dir)? {
        let name = child.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if plumbing.contains(&name.as_str()) {
            continue;
        }
        left.push(child);
    }
    left.sort();
    left.dedup();
    Ok(left)
}

/// One entry, settled against the items on disk.
fn settle(entry: &QuarantineEntry, on_disk: &[PathBuf]) -> QuarantineEntry {
    if entry.status != moving() {
        return entry.clone();
    }
    match entry.stored_path.as_ref() {
        Some(stored) if on_disk.contains(stored) => {
            QuarantineEntry { status: ItemStatus::Quarantined, error: None, ..entry.clone() }
        }
        _ => QuarantineEntry {
            stored_path: None,
            status: ItemStatus::Failed,
            error: Some(ItemErrorCode::IoError),
            ..entry.clone()
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{reconcile, stored_items};
    use crate::model::{ItemErrorCode, ItemStatus, QuarantineEntry, SessionState};
    use crate::quarantine::codes::moving;
    use crate::quarantine::fixtures::{ROOT, entry, session, session_dir, session_fs};
    use crate::testing::FakeFileOps;

    fn stored(sequence: u32, name: &str) -> PathBuf {
        session_dir().join(format!("items/{sequence:04}/{name}"))
    }

    fn with_item(fs: &FakeFileOps, sequence: u32, name: &str) {
        fs.add_file(stored(sequence, name), b"content");
    }

    #[test]
    fn a_session_that_moved_nothing_has_nothing_on_disk() {
        let fs = session_fs();

        assert_eq!(stored_items(&fs, &session_dir()).ok(), Some(Vec::new()));
    }

    #[test]
    fn only_the_items_themselves_count_not_their_sequence_directories() {
        let fs = session_fs();
        with_item(&fs, 1, "app.cache");
        fs.add_dir(session_dir().join("items/0002"));

        let found = stored_items(&fs, &session_dir()).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(found, vec![stored(1, "app.cache")], "an emptied sequence directory holds nothing");
    }

    #[test]
    fn the_plumbing_of_a_session_is_not_something_it_holds() {
        let fs = session_fs();
        crate::quarantine::manifest::write(
            &fs,
            &crate::quarantine::fixtures::manifest_file(),
            &crate::quarantine::manifest::Manifest::new(session(SessionState::Complete, Vec::new())),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        fs.add_file(session_dir().join(".lock"), b"");
        fs.add_dir(session_dir().join("items/0001"));

        assert_eq!(super::leftovers(&fs, &session_dir()).ok(), Some(Vec::new()));

        fs.add_file(session_dir().join("stray"), b"x");
        assert_eq!(
            super::leftovers(&fs, &session_dir()).ok(),
            Some(vec![session_dir().join("stray")]),
            "anything else is content and keeps the session alive"
        );
    }

    #[test]
    fn a_moving_entry_whose_item_arrived_is_quarantined() {
        let fs = session_fs();
        with_item(&fs, 1, "app");
        let interrupted = QuarantineEntry {
            status: moving(),
            stored_path: Some(stored(1, "app")),
            ..entry(1, "/Users/dana/app", 10, ItemStatus::Planned)
        };
        let before = session(SessionState::InProgress, vec![interrupted]);

        let after = reconcile(&before, &session_dir(), &fs).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(after.session.entries[0].status, ItemStatus::Quarantined);
        assert_eq!(after.session.total_bytes, 10);
        assert!(after.orphans.is_empty());
    }

    #[test]
    fn a_moving_entry_whose_item_never_arrived_failed() {
        let fs = session_fs();
        let interrupted = QuarantineEntry {
            status: moving(),
            stored_path: Some(stored(1, "app")),
            ..entry(1, "/Users/dana/app", 10, ItemStatus::Planned)
        };
        let before = session(SessionState::InProgress, vec![interrupted]);

        let after = reconcile(&before, &session_dir(), &fs).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(after.session.entries[0].status, ItemStatus::Failed);
        assert_eq!(after.session.entries[0].error, Some(ItemErrorCode::IoError));
        assert!(after.session.entries[0].stored_path.is_none(), "it is not in the store");
        assert_eq!(after.session.total_bytes, 0);
    }

    #[test]
    fn an_item_no_entry_claims_is_an_orphan() {
        let fs = session_fs();
        with_item(&fs, 9, "mystery");
        let before = session(SessionState::Complete, Vec::new());

        let after = reconcile(&before, &session_dir(), &fs).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(after.orphans, vec![stored(9, "mystery")]);
    }

    #[test]
    fn an_entry_that_is_not_moving_is_left_exactly_as_it_was() {
        let fs = session_fs();
        with_item(&fs, 1, "app");
        let quarantined = QuarantineEntry {
            stored_path: Some(stored(1, "app")),
            ..entry(1, "/Users/dana/app", 10, ItemStatus::Quarantined)
        };
        let before = session(SessionState::Complete, vec![quarantined.clone()]);

        let after = reconcile(&before, &session_dir(), &fs).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(after.session.entries, vec![quarantined]);
        assert!(after.orphans.is_empty());
    }

    #[test]
    fn a_store_without_an_items_directory_reconciles_to_nothing() {
        let fs = session_fs();

        let after = reconcile(&session(SessionState::Complete, Vec::new()), Path::new(ROOT), &fs)
            .unwrap_or_else(|error| panic!("{error}"));

        assert!(after.orphans.is_empty());
    }
}
