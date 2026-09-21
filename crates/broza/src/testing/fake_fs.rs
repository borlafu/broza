//! In-memory [`FileOps`](crate::ports::FileOps).
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

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use jiff::Timestamp;

use crate::BrozaError;
use crate::adapters::io_error::not_found;
use crate::ports::{EntryMetadata, FileOps};
use crate::testing::fake_posix::{
    EISDIR, ENOTDIR, EXDEV, check_allowed, check_parent, check_replaceable, errno_error, expect_buildable,
    from_tree_error, resolve, resolve_parent,
};
use crate::testing::fake_tree::{NodeKind, Tree};
use crate::testing::sync::lock;

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
    tree: Mutex<Tree>,
}

impl Default for FakeFileOps {
    fn default() -> Self {
        Self { tree: Mutex::new(Tree::new()) }
    }
}

impl FakeFileOps {
    /// An empty filesystem with no roots.
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare `path` as a root directory sitting on `device`.
    ///
    /// Everything below a root reports that root's device, so two roots are what a
    /// test needs to exercise a cross-device failure.
    pub fn add_root(&self, path: impl AsRef<Path>, device: u64) {
        lock(&self.tree).add_root(path.as_ref(), device);
    }

    /// Add a file, creating any missing parent directory.
    pub fn add_file(&self, path: impl AsRef<Path>, contents: &[u8]) {
        self.add_node(path.as_ref(), NodeKind::File(contents.to_vec()));
    }

    /// Add a symbolic link to `target`; `metadata` reports it without following it.
    pub fn add_symlink(&self, path: impl AsRef<Path>, target: impl AsRef<Path>) {
        self.add_node(path.as_ref(), NodeKind::Symlink(target.as_ref().to_path_buf()));
    }

    /// Add `link` as a second name for the file at `existing`, sharing its inode.
    ///
    /// # Panics
    ///
    /// When `existing` is missing or is a directory, as the builders do for every
    /// tree the real filesystem could not hold.
    pub fn add_hard_link(&self, existing: impl AsRef<Path>, link: impl AsRef<Path>) {
        let (existing, link) = (existing.as_ref(), link.as_ref());
        let mut tree = lock(&self.tree);
        if let Some(parent) = link.parent() {
            expect_buildable(parent, tree.create_dir_all(parent));
        }
        assert!(
            tree.link(existing, link),
            "cannot hard link {} to {}: not an existing non-directory entry",
            link.display(),
            existing.display()
        );
    }

    /// Builder form of [`FakeFileOps::add_hard_link`].
    #[must_use]
    pub fn with_hard_link(self, existing: impl AsRef<Path>, link: impl AsRef<Path>) -> Self {
        self.add_hard_link(existing, link);
        self
    }

    /// Add a directory and every missing parent.
    pub fn add_dir(&self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        expect_buildable(path, lock(&self.tree).create_dir_all(path));
    }

    /// Give an existing entry an apparent size unrelated to its contents.
    pub fn set_size(&self, path: impl AsRef<Path>, size_bytes: u64) {
        lock(&self.tree).set_size(path.as_ref(), size_bytes);
    }

    /// Refuse every access to `path` and everything below it.
    ///
    /// This is how a test reproduces a volume Broza has no Full Disk Access to:
    /// every call about such a path fails with
    /// [`BrozaError::PermissionDenied`](crate::BrozaError::PermissionDenied).
    pub fn add_denied(&self, path: impl AsRef<Path>) {
        lock(&self.tree).add_denied(path.as_ref());
    }

    /// Builder form of [`FakeFileOps::add_denied`].
    #[must_use]
    pub fn with_denied(self, path: impl AsRef<Path>) -> Self {
        self.add_denied(path);
        self
    }

    /// Set the modification and access times of an existing entry.
    pub fn set_times(&self, path: impl AsRef<Path>, modified: Timestamp, accessed: Timestamp) {
        lock(&self.tree).set_times(path.as_ref(), modified, accessed);
    }

    /// Builder form of [`FakeFileOps::add_root`].
    #[must_use]
    pub fn with_root(self, path: impl AsRef<Path>, device: u64) -> Self {
        self.add_root(path, device);
        self
    }

    /// Builder form of [`FakeFileOps::add_file`].
    #[must_use]
    pub fn with_file(self, path: impl AsRef<Path>, contents: &[u8]) -> Self {
        self.add_file(path, contents);
        self
    }

    /// Builder form of [`FakeFileOps::add_dir`].
    #[must_use]
    pub fn with_dir(self, path: impl AsRef<Path>) -> Self {
        self.add_dir(path);
        self
    }

    /// Builder form of [`FakeFileOps::add_symlink`].
    #[must_use]
    pub fn with_symlink(self, path: impl AsRef<Path>, target: impl AsRef<Path>) -> Self {
        self.add_symlink(path, target);
        self
    }

    /// Add a file whose apparent size is `size_bytes` but that stores no contents.
    #[must_use]
    pub fn with_sized_file(self, path: impl AsRef<Path>, size_bytes: u64) -> Self {
        let path = path.as_ref().to_path_buf();
        self.add_file(&path, &[]);
        self.set_size(&path, size_bytes);
        self
    }

    /// Builder form of [`FakeFileOps::set_times`].
    #[must_use]
    pub fn with_times(self, path: impl AsRef<Path>, modified: Timestamp, accessed: Timestamp) -> Self {
        self.set_times(path, modified, accessed);
        self
    }

    /// Every path in the tree, sorted. For assertions on what a test left behind.
    pub fn paths(&self) -> Vec<PathBuf> {
        lock(&self.tree).paths()
    }

    /// Insert `kind` at `path`, creating the parents a test did not spell out.
    fn add_node(&self, path: &Path, kind: NodeKind) {
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
            modified: node.modified,
            accessed: node.accessed,
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
        let tree = lock(&self.tree);
        !tree.is_denied(path) && resolve_parent(&tree, path).is_ok_and(|resolved| tree.exists(&resolved))
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), BrozaError> {
        let mut tree = lock(&self.tree);
        check_allowed(&tree, from)?;
        check_allowed(&tree, to)?;
        let source = resolve_parent(&tree, from)?;
        let destination = resolve_parent(&tree, to)?;
        if !tree.exists(&source) {
            return Err(not_found(from));
        }
        if tree.device_for(&source) != tree.device_for(&destination) {
            let context = format!("rename {} to {} across devices", from.display(), to.display());
            return Err(errno_error(context, EXDEV));
        }
        check_parent(&tree, &destination)?;
        check_replaceable(&tree, &source, &destination)?;
        tree.remove_subtree(&destination);
        tree.move_subtree(&source, &destination);
        Ok(())
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
