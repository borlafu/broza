//! Measuring a directory immediately before it is moved.
//!
//! The safety kernel verifies the size of *files* only: `lstat` on a directory
//! reports the directory entry, not the subtree, and walking trees inside the
//! guard would double the cost of every run
//! ([`ApprovedItem::size_verified`](crate::safety::guard::ApprovedItem::size_verified)).
//! A directory's planned size therefore comes from the scan and may be stale, so
//! the store re-measures it here and abandons the item when `--max-size` would be
//! exceeded (`docs/cli-spec.md` §3.4, check 6).

use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::ports::FileOps;

/// Bytes held below `root`, `root` included when it is not a directory.
///
/// The figure is `allocated_bytes` — what the volume actually gives back — not
/// the apparent size: a 10 GB sparse file frees the blocks it occupies, and a
/// cap measured in apparent bytes would be wrong in both directions.
///
/// Symbolic links are never followed: a link contributes the size of the link
/// itself, exactly like the `rename` that is about to move it. Directories
/// contribute nothing of their own. A file reachable through several hard links
/// inside the tree is counted once per link, which can only over-estimate and
/// therefore never lets a move slip past the cap.
///
/// `progress` is called with the running total after every entry, so a caller
/// walking a large tree can report what it is doing.
///
/// # Errors
///
/// The first error [`FileOps::metadata`] or [`FileOps::read_dir`] reports. A
/// directory Broza may not read makes the whole measurement fail rather than
/// return a total that is quietly too small.
pub fn measure_dir_bytes(
    fs: &dyn FileOps,
    root: &Path,
    progress: Option<&dyn Fn(u64)>,
) -> Result<u64, BrozaError> {
    let mut pending: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut total = 0_u64;
    while let Some(path) = pending.pop() {
        let entry = fs.metadata(&path)?;
        if entry.is_dir {
            pending.extend(fs.read_dir(&path)?);
            continue;
        }
        total = total.saturating_add(entry.allocated_bytes);
        if let Some(report) = progress {
            report(total);
        }
    }
    Ok(total)
}

/// `true` when adding `size_bytes` to `already_moved` would pass `max_size`.
///
/// No cap always fits. The sum saturates: a total that overflows `u64` is past
/// any cap a caller could have set.
pub fn exceeds_cap(already_moved: u64, size_bytes: u64, max_size: Option<u64>) -> bool {
    max_size.is_some_and(|cap| already_moved.saturating_add(size_bytes) > cap)
}

/// Allocated bytes a removal of `root` actually frees: every file with a single
/// name, once. A file with more than one hard link keeps its blocks alive
/// through the other names, so it frees nothing here (`AGENTS.md` §2.7).
///
/// # Errors
///
/// The first error [`FileOps::metadata`] or [`FileOps::read_dir`] reports.
pub fn measure_freed_bytes(fs: &dyn FileOps, root: &Path) -> Result<u64, BrozaError> {
    let mut pending: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut total = 0_u64;
    while let Some(path) = pending.pop() {
        let entry = fs.metadata(&path)?;
        if entry.is_dir {
            pending.extend(fs.read_dir(&path)?);
            continue;
        }
        if entry.link_count <= 1 {
            total = total.saturating_add(entry.allocated_bytes);
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{exceeds_cap, measure_dir_bytes};
    use crate::BrozaError;
    use crate::testing::FakeFileOps;

    /// Allocation unit of the in-memory filesystem, as APFS reports one.
    const UNIT: u64 = 4096;

    fn tree() -> FakeFileOps {
        FakeFileOps::new()
            .with_root("/Users", 2)
            .with_sized_file("/Users/dana/cache/a", UNIT)
            .with_sized_file("/Users/dana/cache/deep/b", 3 * UNIT)
            .with_sized_file("/Users/dana/cache/deep/deeper/c", 1)
            .with_sized_file("/Users/dana/elsewhere/d", 100 * UNIT)
    }

    #[test]
    fn a_directory_is_worth_the_blocks_the_files_below_it_occupy() {
        let total = measure_dir_bytes(&tree(), Path::new("/Users/dana/cache"), None);

        assert_eq!(total.ok(), Some(5 * UNIT), "a one-byte file still costs a whole unit");
    }

    #[test]
    fn a_file_is_worth_what_it_occupies() {
        let total = measure_dir_bytes(&tree(), Path::new("/Users/dana/cache/a"), None);

        assert_eq!(total.ok(), Some(UNIT));
    }

    #[test]
    fn the_progress_callback_sees_the_running_total() {
        let seen = std::cell::RefCell::new(Vec::new());
        let report = |total: u64| seen.borrow_mut().push(total);

        let total = measure_dir_bytes(&tree(), Path::new("/Users/dana/cache"), Some(&report));

        assert_eq!(total.ok(), Some(5 * UNIT));
        assert_eq!(seen.borrow().len(), 3, "one call per file");
        assert_eq!(seen.borrow().last().copied(), Some(5 * UNIT));
    }

    #[test]
    fn an_empty_directory_is_worth_nothing() {
        let fs = FakeFileOps::new().with_root("/Users", 2).with_dir("/Users/dana/empty");

        assert_eq!(measure_dir_bytes(&fs, Path::new("/Users/dana/empty"), None).ok(), Some(0));
    }

    #[test]
    fn a_symlink_occupies_nothing_and_is_never_followed() {
        let fs = tree();
        fs.add_symlink("/Users/dana/cache/link", "/Users/dana/elsewhere");

        let total = measure_dir_bytes(&fs, Path::new("/Users/dana/cache"), None).ok();

        assert_eq!(total, Some(5 * UNIT), "the link is worth nothing and its target is not counted");
    }

    #[test]
    fn a_directory_broza_may_not_read_fails_the_measurement() {
        let fs = tree().with_denied("/Users/dana/cache/deep");

        let error = measure_dir_bytes(&fs, Path::new("/Users/dana/cache"), None);

        assert!(matches!(error, Err(BrozaError::PermissionDenied { .. })), "{error:?}");
    }

    #[test]
    fn a_missing_directory_is_a_missing_target() {
        let error = measure_dir_bytes(&tree(), Path::new("/Users/dana/ghost"), None);

        assert!(matches!(error, Err(BrozaError::TargetNotFound(_))), "{error:?}");
    }

    #[test]
    fn the_cap_counts_what_was_already_moved() {
        assert!(!exceeds_cap(90, 10, Some(100)), "exactly the cap still fits");
        assert!(exceeds_cap(90, 11, Some(100)));
        assert!(!exceeds_cap(u64::MAX, u64::MAX, None), "no cap always fits");
        assert!(exceeds_cap(u64::MAX, 1, Some(u64::MAX - 1)), "a saturating total is past any cap");
    }
}
