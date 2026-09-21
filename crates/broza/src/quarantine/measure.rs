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

/// Apparent bytes held below `root`, `root` included when it is not a directory.
///
/// Symbolic links are never followed: a link contributes the size of the link
/// itself, exactly like the `rename` that is about to move it. Directories
/// contribute nothing of their own, so the figure is the apparent size of the
/// content, comparable with the scan's aggregate. A file reachable through
/// several hard links inside the tree is counted once per link, which can only
/// over-estimate and therefore never lets a move slip past the cap.
///
/// # Errors
///
/// The first error [`FileOps::metadata`] or [`FileOps::read_dir`] reports. A
/// directory Broza may not read makes the whole measurement fail rather than
/// return a total that is quietly too small.
pub fn measure_dir_bytes(fs: &dyn FileOps, root: &Path) -> Result<u64, BrozaError> {
    let mut pending: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut total = 0_u64;
    while let Some(path) = pending.pop() {
        let entry = fs.metadata(&path)?;
        if !entry.is_dir {
            total = total.saturating_add(entry.size_bytes);
            continue;
        }
        pending.extend(fs.read_dir(&path)?);
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{exceeds_cap, measure_dir_bytes};
    use crate::BrozaError;
    use crate::testing::FakeFileOps;

    fn tree() -> FakeFileOps {
        FakeFileOps::new()
            .with_root("/Users", 2)
            .with_sized_file("/Users/dana/cache/a", 10)
            .with_sized_file("/Users/dana/cache/deep/b", 30)
            .with_sized_file("/Users/dana/cache/deep/deeper/c", 2)
            .with_sized_file("/Users/dana/elsewhere/d", 1000)
    }

    #[test]
    fn a_directory_is_worth_the_sum_of_the_files_below_it() {
        let total = measure_dir_bytes(&tree(), Path::new("/Users/dana/cache"));

        assert_eq!(total.ok(), Some(42));
    }

    #[test]
    fn a_file_is_worth_its_own_size() {
        let total = measure_dir_bytes(&tree(), Path::new("/Users/dana/cache/a"));

        assert_eq!(total.ok(), Some(10));
    }

    #[test]
    fn an_empty_directory_is_worth_nothing() {
        let fs = FakeFileOps::new().with_root("/Users", 2).with_dir("/Users/dana/empty");

        assert_eq!(measure_dir_bytes(&fs, Path::new("/Users/dana/empty")).ok(), Some(0));
    }

    #[test]
    fn a_symlink_counts_as_itself_and_is_never_followed() {
        let fs = tree();
        fs.add_symlink("/Users/dana/cache/link", "/Users/dana/elsewhere");

        let total = measure_dir_bytes(&fs, Path::new("/Users/dana/cache")).ok();

        assert!(total.is_some_and(|bytes| bytes < 1000), "the link target must not be counted: {total:?}");
    }

    #[test]
    fn a_directory_broza_may_not_read_fails_the_measurement() {
        let fs = tree().with_denied("/Users/dana/cache/deep");

        let error = measure_dir_bytes(&fs, Path::new("/Users/dana/cache"));

        assert!(matches!(error, Err(BrozaError::PermissionDenied { .. })), "{error:?}");
    }

    #[test]
    fn a_missing_directory_is_a_missing_target() {
        let error = measure_dir_bytes(&tree(), Path::new("/Users/dana/ghost"));

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
