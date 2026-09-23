//! In-memory [`FileOps`].
//!
//! `FakeFileOps` is the filesystem every unit test runs against: no `$HOME`, no real
//! disks, no cleanup (`AGENTS.md` §7). It mirrors the observable behaviour of
//! [`StdFileOps`](crate::adapters::StdFileOps) down to the errno, because a cleanup
//! tool that mistakes "this is a directory" for "this does not exist" deletes the
//! wrong thing. `crates/broza/tests/fakes_behave_like_std*.rs` keeps the two honest.
//!
//! Symlinks resolve like they do on unix: every call resolves the components before
//! the last one, and only `read` and `read_dir` also follow the last one.
//!
//! The `add_*` and `with_*` setup methods panic when a test describes an impossible
//! tree (a file used as a directory). The [`FileOps`] methods never panic; they
//! return the error the real filesystem would.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::BrozaError;
use crate::adapters::io_error::not_found;
use crate::ports::{EntryMetadata, FileOps, FsLock, RenameMode};
use crate::testing::fake_posix::{
    EEXIST, EISDIR, ENOTDIR, EXDEV, check_allowed, check_parent, check_replaceable, errno_error,
    expect_buildable, from_tree_error, resolve, resolve_parent,
};
use crate::testing::fake_tree::{NodeKind, Tree};
use crate::testing::sync::lock;

mod builders;
mod exclusive;

/// An in-memory filesystem.
///
/// ```
/// # use std::path::Path;
/// # use broza::ports::FileOps;
/// # use broza::testing::FakeFileOps;
/// let fs = FakeFileOps::new().with_root("/data", 2).with_file("/data/a.txt", b"hi");
/// assert!(fs.exists(Path::new("/data/a.txt")));
/// assert_eq!(fs.metadata(Path::new("/data/a.txt")).map(|m| m.device).unwrap_or_default(), 2);
/// ```
#[derive(Debug)]
pub struct FakeFileOps {
    /// The tree, behind a lock so the fake can be shared as `Arc<dyn FileOps>`.
    pub(super) tree: Mutex<Tree>,
    /// Paths currently locked by a live [`FsLock`] guard.
    pub(super) locks: Arc<Mutex<BTreeSet<PathBuf>>>,
    /// Prefixes whose filesystem has no `renamex_np`, as exFAT does not.
    pub(super) no_exclusive_rename: Mutex<Vec<PathBuf>>,
}

impl Default for FakeFileOps {
    fn default() -> Self {
        Self {
            tree: Mutex::new(Tree::new()),
            locks: Arc::new(Mutex::new(BTreeSet::new())),
            no_exclusive_rename: Mutex::new(Vec::new()),
        }
    }
}

impl FakeFileOps {
    /// An empty filesystem with no roots.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `kind` at `path`, creating the parents a test did not spell out.
    pub(super) fn add_node(&self, path: &Path, kind: NodeKind) {
        let mut tree = lock(&self.tree);
        if let Some(parent) = path.parent() {
            expect_buildable(parent, tree.create_dir_all(parent));
        }
        tree.insert(path, kind);
    }
}

impl FileOps for FakeFileOps {
    fn metadata(&self, path: &Path) -> Result<EntryMetadata, BrozaError> {
        let tree = lock(&self.tree);
        check_allowed(&tree, path)?;
        let resolved = resolve_parent(&tree, path)?;
        let node = tree.get(&resolved).ok_or_else(|| not_found(path))?;
        Ok(EntryMetadata {
            device: tree.device_for(&resolved),
            inode: node.inode,
            size_bytes: node.size_bytes(),
            allocated_bytes: node.allocated_bytes(),
            link_count: tree.link_count(&resolved),
            is_dir: matches!(node.kind, NodeKind::Dir),
            is_symlink: matches!(node.kind, NodeKind::Symlink(_)),
            is_dataless: node.is_dataless,
            modified: node.modified,
            accessed: node.accessed,
            clone_id: tree.clone_id(&resolved),
        })
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>, BrozaError> {
        let tree = lock(&self.tree);
        check_allowed(&tree, path)?;
        let resolved = resolve(&tree, path)?;
        if !tree.exists(&resolved) {
            return Err(not_found(path));
        }
        if !tree.is_dir(&resolved) {
            return Err(errno_error(format!("read directory {}", path.display()), ENOTDIR));
        }
        // Re-rooted at the path the caller asked about, as `readdir` reports it.
        Ok(tree
            .children(&resolved)
            .iter()
            .filter_map(|child| child.file_name())
            .map(|name| path.join(name))
            .collect())
    }

    fn exists(&self, path: &Path) -> bool {
        // `lstat` answers for a denied directory itself; only what is *inside*
        // one is unreachable, as on a real filesystem.
        let tree = lock(&self.tree);
        let under_denied = path.parent().is_some_and(|parent| tree.is_denied(parent));
        !under_denied && resolve_parent(&tree, path).is_ok_and(|resolved| tree.exists(&resolved))
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), BrozaError> {
        rename_in(&mut lock(&self.tree), from, to)
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), BrozaError> {
        let mut tree = lock(&self.tree);
        check_allowed(&tree, path)?;
        let resolved = resolve(&tree, path)?;
        tree.create_dir_all(&resolved).map_err(|error| from_tree_error(path, &error))
    }

    fn remove_tree(&self, path: &Path) -> Result<(), BrozaError> {
        let mut tree = lock(&self.tree);
        check_allowed(&tree, path)?;
        let resolved = resolve_parent(&tree, path)?;
        if !tree.exists(&resolved) {
            return Err(not_found(path));
        }
        tree.remove_subtree(&resolved);
        Ok(())
    }

    fn write_atomic(&self, path: &Path, contents: &[u8]) -> Result<(), BrozaError> {
        let mut tree = lock(&self.tree);
        check_allowed(&tree, path)?;
        let resolved = resolve(&tree, path)?;
        if tree.is_dir(&resolved) {
            return Err(errno_error(format!("write {}", path.display()), EISDIR));
        }
        check_parent(&tree, &resolved)?;
        tree.insert(&resolved, NodeKind::File(contents.to_vec()));
        Ok(())
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>, BrozaError> {
        let tree = lock(&self.tree);
        check_allowed(&tree, path)?;
        let resolved = resolve(&tree, path)?;
        let node = tree.get(&resolved).ok_or_else(|| not_found(path))?;
        match &node.kind {
            NodeKind::File(contents) => Ok(contents.clone()),
            // `resolve` already followed every symlink, so only a directory is left.
            NodeKind::Dir | NodeKind::Symlink(_) => {
                Err(errno_error(format!("read {}", path.display()), EISDIR))
            }
        }
    }

    fn create_dir_exclusive(&self, path: &Path) -> Result<(), BrozaError> {
        let mut tree = lock(&self.tree);
        check_allowed(&tree, path)?;
        let resolved = resolve_parent(&tree, path)?;
        check_parent(&tree, &resolved)?;
        if tree.exists(&resolved) {
            return Err(errno_error(format!("create directory {}", path.display()), EEXIST));
        }
        tree.create_dir_all(&resolved).map_err(|error| from_tree_error(path, &error))
    }

    fn rename_exclusive(&self, from: &Path, to: &Path) -> Result<RenameMode, BrozaError> {
        // One lock for the check and the move: taking it twice would leave a
        // window in which another thread could occupy the destination.
        let mut tree = lock(&self.tree);
        check_allowed(&tree, from)?;
        check_allowed(&tree, to)?;
        let source = resolve_parent(&tree, from)?;
        let destination = resolve_parent(&tree, to)?;
        if !tree.exists(&source) {
            return Err(not_found(from));
        }
        if source == destination {
            // `rename(2)` on macOS succeeds and changes nothing; the mode still
            // says what this filesystem can guarantee.
            return Ok(self.rename_mode_for(&destination));
        }
        if tree.exists(&destination) {
            let context = format!("rename {} to {} without replacing it", from.display(), to.display());
            return Err(errno_error(context, EEXIST));
        }
        let mode = self.rename_mode_for(&destination);
        rename_in(&mut tree, from, to)?;
        Ok(mode)
    }

    fn lock_exclusive(&self, path: &Path) -> Result<Box<dyn FsLock>, BrozaError> {
        let mut tree = lock(&self.tree);
        check_allowed(&tree, path)?;
        let resolved = resolve(&tree, path)?;
        check_parent(&tree, &resolved)?;
        if !tree.exists(&resolved) {
            tree.insert(&resolved, NodeKind::File(Vec::new()));
        }
        let mut held = lock(&self.locks);
        if held.contains(&resolved) {
            return Err(BrozaError::Io {
                context: format!("lock {}: another Broza is working on it", path.display()),
                source: std::io::Error::from(std::io::ErrorKind::WouldBlock),
            });
        }
        held.insert(resolved.clone());
        Ok(Box::new(FakeFsLock { locks: Arc::clone(&self.locks), path: resolved }))
    }
}

/// Move a subtree, with the rules `rename(2)` applies. The caller holds the lock.
fn rename_in(tree: &mut Tree, from: &Path, to: &Path) -> Result<(), BrozaError> {
    check_allowed(tree, from)?;
    check_allowed(tree, to)?;
    let source = resolve_parent(tree, from)?;
    let destination = resolve_parent(tree, to)?;
    if !tree.exists(&source) {
        return Err(not_found(from));
    }
    if tree.device_for(&source) != tree.device_for(&destination) {
        let context = format!("rename {} to {} across devices", from.display(), to.display());
        return Err(errno_error(context, EXDEV));
    }
    check_parent(tree, &destination)?;
    check_replaceable(tree, &source, &destination)?;
    tree.remove_subtree(&destination);
    tree.move_subtree(&source, &destination);
    Ok(())
}

/// A lock held in memory, released when the guard is dropped.
struct FakeFsLock {
    /// The set every [`FakeFileOps`] lock lives in.
    pub(super) locks: Arc<Mutex<BTreeSet<PathBuf>>>,
    /// What this guard holds.
    path: PathBuf,
}

impl FsLock for FakeFsLock {}

impl Drop for FakeFsLock {
    fn drop(&mut self) {
        lock(&self.locks).remove(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::FakeFileOps;
    use crate::BrozaError;
    use crate::ports::FileOps;
    use crate::testing::fake_posix::ELOOP;

    fn errno(result: Result<impl std::fmt::Debug, BrozaError>) -> Option<i32> {
        match result.err()? {
            BrozaError::Io { source, .. } => source.raw_os_error(),
            other => panic!("expected BrozaError::Io, got {other:?}"),
        }
    }

    #[test]
    fn a_file_reports_the_device_of_its_root() {
        let fs = FakeFileOps::new().with_root("/a", 5).with_root("/a/b", 6).with_file("/a/b/f", b"x");

        let meta = fs.metadata(Path::new("/a/b/f")).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(meta.device, 6);
    }

    #[test]
    fn a_sized_file_reports_its_declared_size() {
        let fs = FakeFileOps::new().with_root("/a", 1).with_sized_file("/a/big", 4_000_000);

        let meta = fs.metadata(Path::new("/a/big")).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(meta.size_bytes, 4_000_000);
        assert!(fs.read(Path::new("/a/big")).unwrap_or_else(|e| panic!("{e}")).is_empty());
    }

    #[test]
    fn a_symlink_loop_stops_instead_of_hanging() {
        let fs =
            FakeFileOps::new().with_root("/a", 1).with_symlink("/a/l", "/a/m").with_symlink("/a/m", "/a/l");

        assert_eq!(errno(fs.read(Path::new("/a/l"))), Some(ELOOP));
    }

    #[test]
    fn a_dangling_symlink_reads_as_a_missing_target() {
        let fs = FakeFileOps::new().with_root("/a", 1).with_symlink("/a/l", "ghost");

        assert!(matches!(fs.read(Path::new("/a/l")), Err(BrozaError::TargetNotFound(_))));
        assert!(fs.exists(Path::new("/a/l")));
    }

    #[test]
    fn replacing_a_directory_with_a_file_drops_its_children() {
        let fs = FakeFileOps::new().with_root("/a", 1).with_file("/a/d/inner", b"x");

        fs.add_file("/a/d", b"now a file");

        assert_eq!(fs.paths(), vec![PathBuf::from("/"), PathBuf::from("/a"), PathBuf::from("/a/d")]);
    }

    #[test]
    fn a_builder_tree_lists_every_path_it_created() {
        let fs = FakeFileOps::new().with_root("/a", 1).with_dir("/a/b/c").with_file("/a/b/c/f", b"x");

        assert_eq!(
            fs.paths(),
            vec![
                PathBuf::from("/"),
                PathBuf::from("/a"),
                PathBuf::from("/a/b"),
                PathBuf::from("/a/b/c"),
                PathBuf::from("/a/b/c/f"),
            ]
        );
    }

    #[test]
    #[should_panic(expected = "cannot create")]
    fn the_builder_refuses_to_describe_a_file_used_as_a_directory() {
        let _ = FakeFileOps::new().with_root("/a", 1).with_file("/a/f", b"x").with_dir("/a/f/child");
    }

    #[test]
    fn a_denied_prefix_refuses_every_call_about_it() {
        let fs =
            FakeFileOps::new().with_root("/a", 1).with_file("/a/secret/f", b"x").with_denied("/a/secret");

        assert!(matches!(fs.metadata(Path::new("/a/secret/f")), Err(BrozaError::PermissionDenied { .. })));
        assert!(matches!(fs.read(Path::new("/a/secret/f")), Err(BrozaError::PermissionDenied { .. })));
        assert!(matches!(fs.read_dir(Path::new("/a/secret")), Err(BrozaError::PermissionDenied { .. })));
        assert!(matches!(fs.remove_tree(Path::new("/a/secret/f")), Err(BrozaError::PermissionDenied { .. })));
        assert!(!fs.exists(Path::new("/a/secret/f")));
        assert!(fs.exists(Path::new("/a")));
    }

    #[test]
    fn a_hard_link_is_a_second_name_for_one_inode() {
        let fs =
            FakeFileOps::new().with_root("/a", 1).with_file("/a/f", b"1234").with_hard_link("/a/f", "/a/g");

        let original = fs.metadata(Path::new("/a/f")).unwrap_or_else(|e| panic!("{e}"));
        let link = fs.metadata(Path::new("/a/g")).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(original.inode, link.inode);
        assert_eq!(original.link_count, 2);
        assert_eq!(link.size_bytes, 4);
    }

    #[test]
    #[should_panic(expected = "cannot hard link")]
    fn the_builder_refuses_to_hard_link_a_directory() {
        let _ = FakeFileOps::new().with_root("/a", 1).with_dir("/a/d").with_hard_link("/a/d", "/a/e");
    }

    #[test]
    fn times_set_by_a_test_are_reported_by_metadata() {
        let when = "2021-03-04T05:06:07Z".parse().unwrap_or_else(|e| panic!("{e}"));
        let fs = FakeFileOps::new().with_root("/a", 1).with_file("/a/f", b"x").with_times("/a/f", when, when);

        let meta = fs.metadata(Path::new("/a/f")).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(meta.modified, Some(when));
        assert_eq!(meta.accessed, Some(when));
    }
}
