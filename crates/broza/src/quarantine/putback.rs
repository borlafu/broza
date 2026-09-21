//! Putting one quarantined item back where it came from.
//!
//! Broza never overwrites: an entry whose destination already exists is
//! `skipped` with `collision`, and `--to` only changes *where* the item lands,
//! never that rule (`docs/cli-spec.md` §3.5).

use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::model::{ItemErrorCode, ItemStatus, QuarantineEntry};
use crate::ports::FileOps;
use crate::quarantine::entries::sequence_in;
use crate::quarantine::guarded::{Recheck, io_code, recheck};
use crate::quarantine::layout;
use crate::safety::guard::ApprovedItem;

/// Where an entry goes back to.
///
/// Without `--to` that is the path it came from. With `--to` it is
/// `<to>/<seq>_<basename>`: the sequence keeps two entries that share a
/// basename — `node_modules` from two different projects — apart.
pub fn destination(entry: &QuarantineEntry, to: Option<&Path>) -> PathBuf {
    match to {
        None => entry.original_path.clone(),
        Some(directory) => directory.join(alternative_name(entry)),
    }
}

/// `<seq>_<basename>`, the name an entry takes inside a `--to` directory.
fn alternative_name(entry: &QuarantineEntry) -> String {
    let basename = layout::basename(&entry.original_path).to_string_lossy().into_owned();
    format!("{}_{basename}", layout::sequence_label(sequence_in(entry)))
}

/// Move one entry out of the store, and return the entry it became.
///
/// # Errors
///
/// [`BrozaError::Other`] when the token does not cover the stored path. Every
/// other failure belongs to the entry and comes back as a `skipped` or `failed`
/// entry carrying the reason.
pub fn put_back(
    entry: &QuarantineEntry,
    to: Option<&Path>,
    approved: &[ApprovedItem],
    fs: &dyn FileOps,
) -> Result<QuarantineEntry, BrozaError> {
    let Some(stored) = entry.stored_path.clone() else {
        return Ok(refused(entry, ItemStatus::Failed, ItemErrorCode::NotFound));
    };
    let destination = destination(entry, to);
    if fs.exists(&destination) {
        return Ok(refused(entry, ItemStatus::Skipped, ItemErrorCode::Collision));
    }
    if let Recheck::Refused(code) = recheck(approved, &stored, fs)? {
        return Ok(refused(entry, ItemStatus::Failed, code));
    }
    if let Err(error) = make_room(&destination, fs) {
        return Ok(refused(entry, ItemStatus::Failed, io_code(&error)));
    }
    Ok(match fs.rename(&stored, &destination) {
        Ok(()) => restored(entry, destination),
        Err(error) => refused(entry, ItemStatus::Failed, io_code(&error)),
    })
}

/// Recreate the parent directory the item used to live in, if it is gone.
fn make_room(destination: &Path, fs: &dyn FileOps) -> Result<(), BrozaError> {
    match destination.parent() {
        Some(parent) if !fs.exists(parent) => fs.create_dir_all(parent),
        _ => Ok(()),
    }
}

/// The entry an item that went back became: it no longer lives in the store.
fn restored(entry: &QuarantineEntry, destination: PathBuf) -> QuarantineEntry {
    QuarantineEntry {
        stored_path: None,
        restored_to: Some(destination),
        status: ItemStatus::Restored,
        error: None,
        ..entry.clone()
    }
}

/// The entry an item that stayed in the store became.
fn refused(entry: &QuarantineEntry, status: ItemStatus, error: ItemErrorCode) -> QuarantineEntry {
    QuarantineEntry { status, error: Some(error), ..entry.clone() }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{destination, put_back};
    use crate::BrozaError;
    use crate::model::{ItemErrorCode, ItemStatus, QuarantineEntry};
    use crate::ports::FileOps;
    use crate::quarantine::fixtures::{ROOT, entry, session_dir, store_fs};
    use crate::safety::guard::{ApprovedItem, approve_quarantine_write};
    use crate::testing::{FakeFileOps, mac_mount_table};

    const ORIGINAL: &str = "/Users/dana/Library/Caches/app.cache";

    fn quarantined() -> QuarantineEntry {
        entry(1, ORIGINAL, 10, ItemStatus::Quarantined)
    }

    fn stored_path() -> PathBuf {
        session_dir().join("items/0001/app.cache")
    }

    fn tree() -> FakeFileOps {
        store_fs().with_sized_file(stored_path(), 10)
    }

    fn approved(fs: &FakeFileOps) -> Vec<ApprovedItem> {
        approve_quarantine_write(&[stored_path()], Path::new(ROOT), &mac_mount_table(), fs)
            .unwrap_or_else(|error| panic!("{error}"))
            .items()
            .to_vec()
    }

    fn back(fs: &FakeFileOps, to: Option<&Path>) -> QuarantineEntry {
        put_back(&quarantined(), to, &approved(fs), fs).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn an_item_goes_back_to_the_path_it_came_from() {
        let fs = tree();

        let restored = back(&fs, None);

        assert_eq!(restored.status, ItemStatus::Restored);
        assert_eq!(restored.restored_to, Some(PathBuf::from(ORIGINAL)));
        assert!(restored.stored_path.is_none(), "it no longer lives in the store");
        assert!(fs.exists(Path::new(ORIGINAL)));
        assert!(!fs.exists(&stored_path()));
    }

    #[test]
    fn a_missing_parent_directory_is_recreated() {
        let fs = tree();

        back(&fs, None);

        assert!(fs.exists(Path::new("/Users/dana/Library/Caches")));
    }

    #[test]
    fn an_occupied_original_path_is_skipped_rather_than_overwritten() {
        let fs = tree().with_sized_file(ORIGINAL, 999);

        let skipped = back(&fs, None);

        assert_eq!(skipped.status, ItemStatus::Skipped);
        assert_eq!(skipped.error, Some(ItemErrorCode::Collision));
        assert_eq!(fs.metadata(Path::new(ORIGINAL)).map(|meta| meta.size_bytes).ok(), Some(999));
        assert!(fs.exists(&stored_path()), "the item stays in the store");
    }

    #[test]
    fn an_alternative_directory_keeps_the_basename_behind_the_sequence() {
        let fs = tree().with_dir("/Users/dana/Rescued");

        let restored = back(&fs, Some(Path::new("/Users/dana/Rescued")));

        assert_eq!(destination(&quarantined(), Some(Path::new("/x"))), PathBuf::from("/x/0001_app.cache"));
        assert_eq!(restored.restored_to, Some(PathBuf::from("/Users/dana/Rescued/0001_app.cache")));
        assert!(fs.exists(Path::new("/Users/dana/Rescued/0001_app.cache")));
        assert!(!fs.exists(Path::new(ORIGINAL)), "--to never touches the original path");
    }

    #[test]
    fn an_entry_without_a_stored_path_fails_as_missing() {
        let fs = tree();
        let never_moved = QuarantineEntry { stored_path: None, ..quarantined() };

        let failed =
            put_back(&never_moved, None, &approved(&fs), &fs).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(failed.status, ItemStatus::Failed);
        assert_eq!(failed.error, Some(ItemErrorCode::NotFound));
    }

    #[test]
    fn a_stored_item_that_changed_since_the_check_is_not_moved() {
        let fs = tree();
        let items = approved(&fs);
        fs.remove_tree(&stored_path()).unwrap_or_else(|error| panic!("{error}"));
        fs.add_file(stored_path(), b"an impostor");

        let failed = put_back(&quarantined(), None, &items, &fs).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(failed.status, ItemStatus::Failed);
        assert!(!fs.exists(Path::new(ORIGINAL)));
    }

    #[test]
    fn a_stored_path_the_token_does_not_cover_stops_the_restore() {
        let fs = tree();
        let nothing = approve_quarantine_write(&[], Path::new(ROOT), &mac_mount_table(), &fs)
            .unwrap_or_else(|error| panic!("{error}"));

        let error = put_back(&quarantined(), None, nothing.items(), &fs);

        assert!(matches!(error, Err(BrozaError::Other(_))), "{error:?}");
    }
}
