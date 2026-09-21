//! The POSIX rules the in-memory filesystem has to obey.
//!
//! Path resolution and the error each impossible operation produces, kept apart from
//! [`FakeFileOps`](super::FakeFileOps) itself so that the fake reads as a filesystem
//! and this file reads as the rulebook. Every errno here was measured on APFS and is
//! pinned by `crates/broza/tests/fakes_behave_like_std_posix.rs`.

use std::io;
use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::adapters::io_error::not_found;
use crate::testing::fake_tree::{NodeKind, Tree, TreeError};

/// `EEXIST`: a path already taken by something else.
pub(super) const EEXIST: i32 = 17;
/// `EXDEV`: a rename that crosses devices.
pub(super) const EXDEV: i32 = 18;
/// `ENOTDIR`: a non-directory used as a directory.
pub(super) const ENOTDIR: i32 = 20;
/// `EISDIR`: a directory used as a file.
pub(super) const EISDIR: i32 = 21;
/// `ELOOP`: a symlink chain that never ends.
pub(super) const ELOOP: i32 = 62;
/// `ENOTEMPTY`: a directory that still has children.
pub(super) const ENOTEMPTY: i32 = 66;
/// How many symlinks one path resolution may follow.
pub(super) const MAX_SYMLINK_HOPS: usize = 8;

/// Resolve every symlink on `path`, last component included.
pub(super) fn resolve(tree: &Tree, path: &Path) -> Result<PathBuf, BrozaError> {
    let mut resolved = PathBuf::new();
    let mut hops = 0_usize;
    for component in path.components() {
        resolved.push(component);
        while let Some(target) = symlink_target(tree, &resolved) {
            hops += 1;
            if hops > MAX_SYMLINK_HOPS {
                return Err(errno_error(format!("resolve {}", path.display()), ELOOP));
            }
            resolved = match resolved.parent() {
                Some(parent) if target.is_relative() => parent.join(target),
                _ => target,
            };
        }
    }
    Ok(resolved)
}

/// Resolve every symlink on `path` except the last component, as `lstat` does.
pub(super) fn resolve_parent(tree: &Tree, path: &Path) -> Result<PathBuf, BrozaError> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Ok(path.to_path_buf());
    };
    Ok(resolve(tree, parent)?.join(name))
}

/// Refuse a path the test declared unreadable.
pub(super) fn check_allowed(tree: &Tree, path: &Path) -> Result<(), BrozaError> {
    if tree.is_denied(path) {
        return Err(BrozaError::PermissionDenied { path: path.to_path_buf() });
    }
    Ok(())
}

/// Check that `path` can receive a new entry: its parent must be a directory.
pub(super) fn check_parent(tree: &Tree, path: &Path) -> Result<(), BrozaError> {
    let Some(parent) = path.parent() else { return Ok(()) };
    if !tree.exists(parent) {
        return Err(not_found(parent));
    }
    if !tree.is_dir(parent) {
        return Err(errno_error(format!("open {}", path.display()), ENOTDIR));
    }
    Ok(())
}

/// Target of `path` when it is a symlink.
fn symlink_target(tree: &Tree, path: &Path) -> Option<PathBuf> {
    match tree.get(path).map(|node| &node.kind) {
        Some(NodeKind::Symlink(target)) => Some(target.clone()),
        _ => None,
    }
}

/// Apply the POSIX rules for replacing `destination` with `source`.
pub(super) fn check_replaceable(tree: &Tree, source: &Path, destination: &Path) -> Result<(), BrozaError> {
    if !tree.exists(destination) {
        return Ok(());
    }
    let context = format!("rename {} to {}", source.display(), destination.display());
    match (tree.is_dir(source), tree.is_dir(destination)) {
        (true, false) => Err(errno_error(context, ENOTDIR)),
        (false, true) => Err(errno_error(context, EISDIR)),
        (true, true) if !tree.is_empty_dir(destination) => Err(errno_error(context, ENOTEMPTY)),
        _ => Ok(()),
    }
}

/// Translate a tree failure into the error the real filesystem would report.
pub(super) fn from_tree_error(path: &Path, error: &TreeError) -> BrozaError {
    let context = format!("create directory {}", path.display());
    match error {
        TreeError::NotADirectory(_) => errno_error(context, ENOTDIR),
        TreeError::AlreadyExists(_) => errno_error(context, EEXIST),
    }
}

/// Build the error a given `errno` produces, with the same shape `std` would give.
pub(super) fn errno_error(context: String, errno: i32) -> BrozaError {
    BrozaError::Io { context, source: io::Error::from_raw_os_error(errno) }
}

/// Panic when a test asks the builder for a tree that cannot exist.
pub(super) fn expect_buildable(path: &Path, result: Result<(), TreeError>) {
    if let Err(error) = result {
        panic!("FakeFileOps cannot create {}: {error:?}", path.display());
    }
}
