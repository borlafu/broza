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

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use jiff::Timestamp;

use crate::BrozaError;
use crate::adapters::io_error::not_found;
use crate::ports::{EntryMetadata, FileOps};
use crate::testing::fake_tree::{NodeKind, Tree, TreeError};
use crate::testing::sync::lock;

/// `EEXIST`: a path already taken by something else.
const EEXIST: i32 = 17;
/// `EXDEV`: a rename that crosses devices.
const EXDEV: i32 = 18;
/// `ENOTDIR`: a non-directory used as a directory.
const ENOTDIR: i32 = 20;
/// `EISDIR`: a directory used as a file.
const EISDIR: i32 = 21;
/// `ELOOP`: a symlink chain that never ends.
const ELOOP: i32 = 62;
/// `ENOTEMPTY`: a directory that still has children.
const ENOTEMPTY: i32 = 66;
/// How many symlinks one path resolution may follow.
const MAX_SYMLINK_HOPS: usize = 8;

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

    /// Add a directory and every missing parent.
    pub fn add_dir(&self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        expect_buildable(path, lock(&self.tree).create_dir_all(path));
    }

    /// Give an existing entry an apparent size unrelated to its contents.
    pub fn set_size(&self, path: impl AsRef<Path>, size_bytes: u64) {
        lock(&self.tree).set_size(path.as_ref(), size_bytes);
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

    /// Resolve every symlink on `path`, last component included.
    fn resolve(tree: &Tree, path: &Path) -> Result<PathBuf, BrozaError> {
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
    fn resolve_parent(tree: &Tree, path: &Path) -> Result<PathBuf, BrozaError> {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Ok(path.to_path_buf());
        };
        Ok(Self::resolve(tree, parent)?.join(name))
    }

    /// Check that `path` can receive a new entry: its parent must be a directory.
    fn check_parent(tree: &Tree, path: &Path) -> Result<(), BrozaError> {
        let Some(parent) = path.parent() else { return Ok(()) };
        if !tree.exists(parent) {
            return Err(not_found(parent));
        }
        if !tree.is_dir(parent) {
            return Err(errno_error(format!("open {}", path.display()), ENOTDIR));
        }
        Ok(())
    }
}

impl FileOps for FakeFileOps {
    fn metadata(&self, path: &Path) -> Result<EntryMetadata, BrozaError> {
        let tree = lock(&self.tree);
        let resolved = Self::resolve_parent(&tree, path)?;
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
        let resolved = Self::resolve(&tree, path)?;
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
        Self::resolve_parent(&tree, path).is_ok_and(|resolved| tree.exists(&resolved))
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), BrozaError> {
        let mut tree = lock(&self.tree);
        let source = Self::resolve_parent(&tree, from)?;
        let destination = Self::resolve_parent(&tree, to)?;
        if !tree.exists(&source) {
            return Err(not_found(from));
        }
        if tree.device_for(&source) != tree.device_for(&destination) {
            let context = format!("rename {} to {} across devices", from.display(), to.display());
            return Err(errno_error(context, EXDEV));
        }
        Self::check_parent(&tree, &destination)?;
        check_replaceable(&tree, &source, &destination)?;
        tree.remove_subtree(&destination);
        tree.move_subtree(&source, &destination);
        Ok(())
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), BrozaError> {
        let mut tree = lock(&self.tree);
        let resolved = Self::resolve(&tree, path)?;
        tree.create_dir_all(&resolved).map_err(|error| from_tree_error(path, &error))
    }

    fn remove_tree(&self, path: &Path) -> Result<(), BrozaError> {
        let mut tree = lock(&self.tree);
        let resolved = Self::resolve_parent(&tree, path)?;
        if !tree.exists(&resolved) {
            return Err(not_found(path));
        }
        tree.remove_subtree(&resolved);
        Ok(())
    }

    fn write_atomic(&self, path: &Path, contents: &[u8]) -> Result<(), BrozaError> {
        let mut tree = lock(&self.tree);
        let resolved = Self::resolve(&tree, path)?;
        if tree.is_dir(&resolved) {
            return Err(errno_error(format!("write {}", path.display()), EISDIR));
        }
        Self::check_parent(&tree, &resolved)?;
        tree.insert(&resolved, NodeKind::File(contents.to_vec()));
        Ok(())
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>, BrozaError> {
        let tree = lock(&self.tree);
        let resolved = Self::resolve(&tree, path)?;
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

/// Target of `path` when it is a symlink.
fn symlink_target(tree: &Tree, path: &Path) -> Option<PathBuf> {
    match tree.get(path).map(|node| &node.kind) {
        Some(NodeKind::Symlink(target)) => Some(target.clone()),
        _ => None,
    }
}

/// Apply the POSIX rules for replacing `destination` with `source`.
fn check_replaceable(tree: &Tree, source: &Path, destination: &Path) -> Result<(), BrozaError> {
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
fn from_tree_error(path: &Path, error: &TreeError) -> BrozaError {
    let context = format!("create directory {}", path.display());
    match error {
        TreeError::NotADirectory(_) => errno_error(context, ENOTDIR),
        TreeError::AlreadyExists(_) => errno_error(context, EEXIST),
    }
}

/// Build the error a given `errno` produces, with the same shape `std` would give.
fn errno_error(context: String, errno: i32) -> BrozaError {
    BrozaError::Io { context, source: io::Error::from_raw_os_error(errno) }
}

/// Panic when a test asks the builder for a tree that cannot exist.
fn expect_buildable(path: &Path, result: Result<(), TreeError>) {
    if let Err(error) = result {
        panic!("FakeFileOps cannot create {}: {error:?}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{ELOOP, FakeFileOps};
    use crate::BrozaError;
    use crate::ports::FileOps;

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
    fn times_set_by_a_test_are_reported_by_metadata() {
        let when = "2021-03-04T05:06:07Z".parse().unwrap_or_else(|e| panic!("{e}"));
        let fs = FakeFileOps::new().with_root("/a", 1).with_file("/a/f", b"x").with_times("/a/f", when, when);

        let meta = fs.metadata(Path::new("/a/f")).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(meta.modified, Some(when));
        assert_eq!(meta.accessed, Some(when));
    }
}
