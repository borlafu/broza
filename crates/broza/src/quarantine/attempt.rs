//! One item's journey into the store, and what it became.
//!
//! This is where the time-of-check/time-of-use contract of
//! [`crate::ports::FileOps`] is honoured: the path is `lstat`ed again
//! and its `(device, inode)` compared with the pair the guard recorded, before
//! anything is renamed (`AGENTS.md` §4).

use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::model::{ItemErrorCode, ItemStatus, QuarantineEntry};
use crate::ports::FileOps;
use crate::quarantine::codes::{changed_since_check, max_size_exceeded};
use crate::quarantine::layout;
use crate::quarantine::measure::{exceeds_cap, measure_dir_bytes};
use crate::safety::guard::ApprovedItem;

/// What one item turned into.
pub enum Attempt {
    /// The item is now inside the store.
    Moved {
        /// Where it landed.
        stored: PathBuf,
        /// What it measured immediately before the move.
        size_bytes: u64,
    },
    /// The item stayed where it was.
    Refused {
        /// `skipped` (never attempted) or `failed` (attempted and failed).
        status: ItemStatus,
        /// Why.
        error: ItemErrorCode,
    },
}

/// Where one item is going, and the limits that apply to it.
pub struct Destination<'a> {
    /// Directory of the session receiving the item.
    pub session_dir: &'a Path,
    /// Sequence number of the item inside the session.
    pub sequence: u32,
    /// Device the store sits on; another device cannot be reached by `rename`.
    pub root_device: u64,
    /// `--max-size` cap in bytes.
    pub max_size: Option<u64>,
}

/// Re-check the item, measure it, and move it into the store.
///
/// Never returns an error: a failure of one item is the item's outcome, and the
/// run continues with the next one (`docs/cli-spec.md` §3.4).
pub fn attempt_move(
    item: &ApprovedItem,
    destination: &Destination<'_>,
    moved_bytes: u64,
    fs: &dyn FileOps,
) -> Attempt {
    let source = item.path();
    let current = match fs.metadata(source) {
        Ok(current) => current,
        Err(error) => return failed_io(&error),
    };
    if current.device != item.device() || current.inode != item.inode() {
        return Attempt::Refused { status: ItemStatus::Failed, error: changed_since_check() };
    }
    if current.device != destination.root_device {
        return Attempt::Refused { status: ItemStatus::Skipped, error: ItemErrorCode::CrossVolume };
    }
    let size_bytes = match measured_size(item, fs) {
        Ok(size_bytes) => size_bytes,
        Err(error) => return failed_io(&error),
    };
    if exceeds_cap(moved_bytes, size_bytes, destination.max_size) {
        return Attempt::Refused { status: ItemStatus::Skipped, error: max_size_exceeded() };
    }
    move_into(item, destination, size_bytes, fs)
}

/// Create the item directory and rename the item into it.
fn move_into(
    item: &ApprovedItem,
    destination: &Destination<'_>,
    size_bytes: u64,
    fs: &dyn FileOps,
) -> Attempt {
    let source = item.path();
    let stored = layout::stored_path(destination.session_dir, destination.sequence, layout::basename(source));
    let item_dir = layout::item_dir(destination.session_dir, destination.sequence);
    match fs.create_dir_all(&item_dir).and_then(|()| fs.rename(source, &stored)) {
        Ok(()) => Attempt::Moved { stored, size_bytes },
        Err(error) => failed_io(&error),
    }
}

/// A directory is measured now; a file keeps the size the guard verified.
fn measured_size(item: &ApprovedItem, fs: &dyn FileOps) -> Result<u64, BrozaError> {
    if item.size_verified() {
        return Ok(item.size_bytes());
    }
    measure_dir_bytes(fs, item.path())
}

/// An I/O failure of one item, mapped to an item error code of §4.1.
fn failed_io(error: &BrozaError) -> Attempt {
    let error = match error {
        BrozaError::PermissionDenied { .. } => ItemErrorCode::PermissionDenied,
        BrozaError::TargetNotFound(_) => ItemErrorCode::NotFound,
        _ => ItemErrorCode::IoError,
    };
    Attempt::Refused { status: ItemStatus::Failed, error }
}

/// The status and error code an attempt records.
pub fn outcome_of(attempt: &Attempt) -> (ItemStatus, Option<ItemErrorCode>) {
    match attempt {
        Attempt::Moved { .. } => (ItemStatus::Quarantined, None),
        Attempt::Refused { status, error } => (status.clone(), Some(error.clone())),
    }
}

/// Bytes an attempt added to the store.
pub fn moved_of(attempt: &Attempt) -> u64 {
    match attempt {
        Attempt::Moved { size_bytes, .. } => *size_bytes,
        Attempt::Refused { .. } => 0,
    }
}

/// The entry an attempt produces from the one the manifest already holds.
pub fn updated_entry(entry: &QuarantineEntry, attempt: &Attempt) -> QuarantineEntry {
    let (status, error) = outcome_of(attempt);
    match attempt {
        Attempt::Moved { stored, size_bytes } => QuarantineEntry {
            stored_path: Some(stored.clone()),
            size_bytes: *size_bytes,
            status,
            error,
            ..entry.clone()
        },
        Attempt::Refused { .. } => QuarantineEntry { status, error, ..entry.clone() },
    }
}

#[cfg(test)]
mod tests {
    use super::{Attempt, failed_io, moved_of, outcome_of, updated_entry};
    use crate::BrozaError;
    use crate::model::{ItemErrorCode, ItemStatus};
    use crate::quarantine::fixtures::entry;

    fn refused(error: &BrozaError) -> ItemErrorCode {
        match failed_io(error) {
            Attempt::Refused { error, .. } => error,
            Attempt::Moved { .. } => panic!("a failure never moves anything"),
        }
    }

    #[test]
    fn an_io_failure_keeps_the_reason_the_filesystem_gave() {
        let denied = BrozaError::PermissionDenied { path: "/Users/dana/a".into() };
        let missing = BrozaError::TargetNotFound("/Users/dana/a".to_owned());
        let other = BrozaError::Other("disk on fire".to_owned());

        assert_eq!(refused(&denied), ItemErrorCode::PermissionDenied);
        assert_eq!(refused(&missing), ItemErrorCode::NotFound);
        assert_eq!(refused(&other), ItemErrorCode::IoError);
    }

    #[test]
    fn a_moved_item_records_where_it_landed_and_what_it_measured() {
        let attempt = Attempt::Moved { stored: "/store/0001/a".into(), size_bytes: 77 };
        let before = entry(1, "/Users/dana/a", 10, ItemStatus::Planned);

        let after = updated_entry(&before, &attempt);

        assert_eq!(after.status, ItemStatus::Quarantined);
        assert_eq!(after.size_bytes, 77, "the measured size replaces the planned one");
        assert_eq!(after.stored_path, Some("/store/0001/a".into()));
        assert!(after.error.is_none());
        assert_eq!(moved_of(&attempt), 77);
    }

    #[test]
    fn a_refused_item_keeps_its_planned_size_and_has_no_stored_path() {
        let attempt = Attempt::Refused { status: ItemStatus::Skipped, error: ItemErrorCode::CrossVolume };
        let before = entry(1, "/Users/dana/a", 10, ItemStatus::Planned);

        let after = updated_entry(&before, &attempt);

        assert_eq!(outcome_of(&attempt), (ItemStatus::Skipped, Some(ItemErrorCode::CrossVolume)));
        assert_eq!(after.size_bytes, 10);
        assert!(after.stored_path.is_none());
        assert_eq!(moved_of(&attempt), 0);
    }
}
