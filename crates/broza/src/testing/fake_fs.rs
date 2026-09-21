//! In-memory [`FileOps`](crate::ports::FileOps).
//!
//! `FakeFileOps` is the filesystem every unit test runs against: no `$HOME`, no real
//! disks, no cleanup (`AGENTS.md` §7). It mirrors the observable behaviour of
//! [`StdFileOps`](crate::adapters::StdFileOps), and
//! `crates/broza/tests/fakes_behave_like_std.rs` keeps the two honest.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use jiff::Timestamp;

use crate::BrozaError;
use crate::adapters::io_error::not_found;
use crate::ports::{EntryMetadata, FileOps};
use crate::testing::fake_tree::{NodeKind, Tree};

/// `EXDEV`: the errno a rename across devices fails with.
const EXDEV: i32 = 18;
/// `ENOTDIR`: the errno for treating a non-directory as a directory.
const ENOTDIR: i32 = 20;
/// `EISDIR`: the errno for reading a directory as a file.
const EISDIR: i32 = 21;
/// `ELOOP`: the errno for a symlink chain that never ends.
const ELOOP: i32 = 62;
/// How many symlinks `read` and `read_dir` follow before giving up.
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
        let path = path.as_ref();
        let mut tree = lock(&self.tree);
        if let Some(parent) = path.parent() {
            tree.create_dir_all(parent);
        }
        tree.insert(path, NodeKind::File(contents.to_vec()));
    }

    /// Add a symbolic link to `target`; `metadata` reports it without following it.
    pub fn add_symlink(&self, path: impl AsRef<Path>, target: impl AsRef<Path>) {
        let path = path.as_ref();
        let mut tree = lock(&self.tree);
        if let Some(parent) = path.parent() {
            tree.create_dir_all(parent);
        }
        tree.insert(path, NodeKind::Symlink(target.as_ref().to_path_buf()));
    }

    /// Add a directory and every missing parent.
    pub fn add_dir(&self, path: impl AsRef<Path>) {
        lock(&self.tree).create_dir_all(path.as_ref());
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

    /// Follow up to [`MAX_SYMLINK_HOPS`] symlinks, as an open or a `readdir` would.
    fn resolve(tree: &Tree, path: &Path) -> Result<PathBuf, BrozaError> {
        let mut current = path.to_path_buf();
        for _ in 0..MAX_SYMLINK_HOPS {
            let Some(node) = tree.get(&current) else { return Ok(current) };
            let NodeKind::Symlink(target) = &node.kind else { return Ok(current) };
            current = match current.parent() {
                Some(parent) if target.is_relative() => parent.join(target),
                _ => target.clone(),
            };
        }
        Err(errno_error(format!("resolve {}", path.display()), ELOOP))
    }
}

impl FileOps for FakeFileOps {
    fn metadata(&self, path: &Path) -> Result<EntryMetadata, BrozaError> {
        let tree = lock(&self.tree);
        let node = tree.get(path).ok_or_else(|| not_found(path))?;
        Ok(EntryMetadata {
            device: tree.device_for(path),
            inode: node.inode,
            size_bytes: node.size_bytes(),
            allocated_bytes: node.allocated_bytes(),
            link_count: tree.link_count(path),
            is_dir: matches!(node.kind, NodeKind::Dir),
            is_symlink: matches!(node.kind, NodeKind::Symlink(_)),
            modified: node.modified,
            accessed: node.accessed,
        })
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>, BrozaError> {
        let tree = lock(&self.tree);
        let resolved = Self::resolve(&tree, path)?;
        if tree.get(&resolved).is_none() {
            return Err(not_found(path));
        }
        if !tree.is_dir(&resolved) {
            return Err(errno_error(format!("read directory {}", path.display()), ENOTDIR));
        }
        Ok(tree.children(&resolved))
    }

    fn exists(&self, path: &Path) -> bool {
        lock(&self.tree).get(path).is_some()
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), BrozaError> {
        let mut tree = lock(&self.tree);
        if tree.get(from).is_none() {
            return Err(not_found(from));
        }
        if tree.device_for(from) != tree.device_for(to) {
            let context = format!("rename {} to {} across devices", from.display(), to.display());
            return Err(errno_error(context, EXDEV));
        }
        match to.parent() {
            Some(parent) if !tree.is_dir(parent) => return Err(not_found(parent)),
            _ => {}
        }
        tree.move_subtree(from, to);
        Ok(())
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), BrozaError> {
        lock(&self.tree).create_dir_all(path);
        Ok(())
    }

    fn remove_tree(&self, path: &Path) -> Result<(), BrozaError> {
        let mut tree = lock(&self.tree);
        if tree.get(path).is_none() {
            return Err(not_found(path));
        }
        tree.remove_subtree(path);
        Ok(())
    }

    fn write_atomic(&self, path: &Path, contents: &[u8]) -> Result<(), BrozaError> {
        let mut tree = lock(&self.tree);
        match path.parent() {
            Some(parent) if !tree.is_dir(parent) => return Err(not_found(parent)),
            _ => {}
        }
        tree.insert(path, NodeKind::File(contents.to_vec()));
        Ok(())
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>, BrozaError> {
        let tree = lock(&self.tree);
        let resolved = Self::resolve(&tree, path)?;
        let node = tree.get(&resolved).ok_or_else(|| not_found(path))?;
        match &node.kind {
            NodeKind::File(contents) => Ok(contents.clone()),
            NodeKind::Dir => Err(errno_error(format!("read {}", path.display()), EISDIR)),
            NodeKind::Symlink(_) => Err(errno_error(format!("read {}", path.display()), ELOOP)),
        }
    }
}

/// Build the error a given `errno` produces, with the same shape `std` would give.
fn errno_error(context: String, errno: i32) -> BrozaError {
    BrozaError::Io { context, source: io::Error::from_raw_os_error(errno) }
}

/// Lock a mutex, recovering the value when another test thread poisoned it.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{EISDIR, ELOOP, ENOTDIR, FakeFileOps};
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
    fn reading_through_a_symlink_follows_it_but_metadata_does_not() {
        let fs = FakeFileOps::new().with_root("/a", 1).with_file("/a/f", b"body").with_symlink("/a/l", "f");

        assert_eq!(fs.read(Path::new("/a/l")).unwrap_or_else(|e| panic!("{e}")), b"body");
        let meta = fs.metadata(Path::new("/a/l")).unwrap_or_else(|e| panic!("{e}"));
        assert!(meta.is_symlink);
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
    fn a_directory_cannot_be_read_as_a_file() {
        let fs = FakeFileOps::new().with_root("/a", 1);

        assert_eq!(errno(fs.read(Path::new("/a"))), Some(EISDIR));
    }

    #[test]
    fn a_file_cannot_be_read_as_a_directory() {
        let fs = FakeFileOps::new().with_root("/a", 1).with_file("/a/f", b"x");

        assert_eq!(errno(fs.read_dir(Path::new("/a/f"))), Some(ENOTDIR));
    }

    #[test]
    fn writing_into_a_missing_directory_fails() {
        let fs = FakeFileOps::new().with_root("/a", 1);

        let err = fs.write_atomic(Path::new("/a/missing/f"), b"x").err();

        assert!(matches!(err, Some(BrozaError::TargetNotFound(_))), "{err:?}");
    }

    #[test]
    fn renaming_a_missing_entry_reports_the_source_as_not_found() {
        let fs = FakeFileOps::new().with_root("/a", 1);

        let err = fs.rename(Path::new("/a/ghost"), Path::new("/a/f")).err();

        assert!(matches!(err, Some(BrozaError::TargetNotFound(_))), "{err:?}");
    }

    #[test]
    fn renaming_into_a_missing_directory_fails_and_keeps_the_source() {
        let fs = FakeFileOps::new().with_root("/a", 1).with_file("/a/f", b"x");

        let err = fs.rename(Path::new("/a/f"), Path::new("/a/missing/f")).err();

        assert!(matches!(err, Some(BrozaError::TargetNotFound(_))), "{err:?}");
        assert!(fs.exists(Path::new("/a/f")));
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
    fn times_set_by_a_test_are_reported_by_metadata() {
        let when = "2021-03-04T05:06:07Z".parse().unwrap_or_else(|e| panic!("{e}"));
        let fs = FakeFileOps::new().with_root("/a", 1).with_file("/a/f", b"x").with_times("/a/f", when, when);

        let meta = fs.metadata(Path::new("/a/f")).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(meta.modified, Some(when));
        assert_eq!(meta.accessed, Some(when));
    }
}
