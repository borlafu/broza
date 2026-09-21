//! Path validation without following symlinks (`docs/cli-spec.md` §3.4, check 2).
//!
//! # Why `..` is refused instead of resolved
//!
//! Collapsing `a/evil/../b` lexically deletes `evil` from the path, so the symlink
//! check never sees it — and the kernel would then approve a path that walks
//! *through* a symlink at run time. Broza therefore refuses any `.` or `..`
//! component: detectors and the CLI hand over already-resolved absolute paths.
//!
//! The allowlist those paths are checked against lives in [`crate::safety::roots`].

use std::path::Path;
use std::path::PathBuf;

use crate::ports::{EntryMetadata, FileOps};
use crate::safety::rejection::GuardRejection;

/// A path that passed check 2, together with the `lstat` that proved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalPath {
    /// The validated path: absolute, free of `.`/`..`, no symlinked component.
    pub path: PathBuf,
    /// Metadata of the final component, from the same `lstat` the check used.
    pub metadata: EntryMetadata,
}

/// Validates `path` and proves that no component of it is a symbolic link.
///
/// Rejects relative paths, any `.` or `..` component, and any component that
/// `lstat` reports as a symlink. Returns the metadata of the final component so
/// the caller can record `(device, inode)` without a second `lstat`.
pub fn canonicalize_no_follow(path: &Path, fs: &dyn FileOps) -> Result<CanonicalPath, GuardRejection> {
    reject_relative_components(path)?;
    let metadata = reject_symlinked_components(path, fs)?;
    Ok(CanonicalPath { path: path.to_path_buf(), metadata })
}

/// Check 2a: the path must be absolute and already resolved.
///
/// The raw bytes are inspected on purpose: [`Path::components`] silently drops a
/// `.` in the middle of a path, and the whole point here is to see it.
pub(crate) fn reject_relative_components(path: &Path) -> Result<(), GuardRejection> {
    use std::os::unix::ffi::OsStrExt;

    if !path.is_absolute() {
        return Err(GuardRejection::NotAbsolute(path.to_path_buf()));
    }
    let relative =
        path.as_os_str().as_bytes().split(|byte| *byte == b'/').any(|part| part == b"." || part == b"..");
    if relative {
        return Err(GuardRejection::RelativeComponent(path.to_path_buf()));
    }
    Ok(())
}

/// Check 2b: `lstat` every component from the root down, refusing the first symlink.
fn reject_symlinked_components(path: &Path, fs: &dyn FileOps) -> Result<EntryMetadata, GuardRejection> {
    let root = Path::new("/");
    let mut ancestors: Vec<&Path> = path.ancestors().filter(|entry| *entry != root).collect();
    ancestors.reverse();
    let mut leaf = None;
    for ancestor in ancestors {
        let metadata = fs.metadata(ancestor).map_err(|error| GuardRejection::unreadable(ancestor, &error))?;
        if metadata.is_symlink {
            return Err(GuardRejection::SymlinkComponent(ancestor.to_path_buf()));
        }
        leaf = Some(metadata);
    }
    leaf.ok_or_else(|| GuardRejection::RootItself(path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::canonicalize_no_follow;
    use crate::safety::rejection::GuardRejection;
    use crate::testing::FakeFileOps;
    use std::path::{Path, PathBuf};

    /// A home on the Data volume with two symlinks in it.
    fn tree() -> FakeFileOps {
        FakeFileOps::new()
            .with_root("/", 1)
            .with_root("/Users", 2)
            .with_dir("/Users/dana/Library/Caches")
            .with_sized_file("/Users/dana/Library/Caches/app.cache", 10)
            .with_symlink("/Users/dana/Library/Caches/link", "/Users/dana/Library/Caches/app.cache")
            .with_symlink("/Users/dana/Library/evil", "/Users/dana/Library")
            .with_dir("/Users/dana/target")
            .with_symlink("/Users/dana/linked", "/Users/dana/target")
    }

    #[test]
    fn a_relative_path_is_refused() {
        let error = canonicalize_no_follow(Path::new("Library/Caches"), &tree());
        assert_eq!(error, Err(GuardRejection::NotAbsolute("Library/Caches".into())));
    }

    /// Regression: `..` used to be collapsed before the `lstat` walk, so the
    /// symlink the `..` jumped over was never checked.
    #[test]
    fn a_dot_dot_that_jumps_over_a_symlink_is_refused() {
        let sneaky = Path::new("/Users/dana/Library/evil/../Caches/app.cache");
        let error = canonicalize_no_follow(sneaky, &tree());
        assert_eq!(error, Err(GuardRejection::RelativeComponent(sneaky.to_path_buf())));
    }

    #[test]
    fn a_single_dot_is_refused_too() {
        let path = Path::new("/Users/dana/Library/./Caches/app.cache");
        assert_eq!(
            canonicalize_no_follow(path, &tree()),
            Err(GuardRejection::RelativeComponent(path.to_path_buf()))
        );
    }

    #[test]
    fn a_plain_path_keeps_its_spelling_and_reports_its_identity() {
        let resolved = canonicalize_no_follow(Path::new("/Users/dana/Library/Caches/app.cache"), &tree())
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(resolved.path, PathBuf::from("/Users/dana/Library/Caches/app.cache"));
        assert_eq!(resolved.metadata.size_bytes, 10);
        assert_eq!(resolved.metadata.device, 2, "the home lives on the Data volume");
        assert!(resolved.metadata.inode > 0);
    }

    #[test]
    fn a_symlinked_leaf_is_refused() {
        let error = canonicalize_no_follow(Path::new("/Users/dana/Library/Caches/link"), &tree());
        assert_eq!(error, Err(GuardRejection::SymlinkComponent("/Users/dana/Library/Caches/link".into())));
    }

    #[test]
    fn a_symlinked_intermediate_component_is_refused_by_name() {
        let error = canonicalize_no_follow(Path::new("/Users/dana/linked/inside.cache"), &tree());
        assert_eq!(error, Err(GuardRejection::SymlinkComponent("/Users/dana/linked".into())));
    }

    #[test]
    fn a_path_that_cannot_be_stat_ed_reports_why() {
        let error = canonicalize_no_follow(Path::new("/Users/dana/gone"), &tree())
            .err()
            .unwrap_or_else(|| panic!("a missing path must be reported"));
        assert!(error.is_missing_path(), "{error}");
    }

    #[test]
    fn the_filesystem_root_is_never_a_target() {
        assert_eq!(
            canonicalize_no_follow(Path::new("/"), &tree()),
            Err(GuardRejection::RootItself("/".into()))
        );
    }
}
