//! How a test describes a tree to [`FakeFileOps`](super::FakeFileOps).
//!
//! Every method here is setup, not behaviour: the `add_*` pair mutates through
//! a shared reference, the `with_*` pair chains. They panic when a test
//! describes a tree the real filesystem could not hold, because that is a
//! mistake in the test and hiding it would waste somebody's afternoon.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::testing::fake_fs::FakeFileOps;
use crate::testing::fake_posix::expect_buildable;
use crate::testing::fake_tree::NodeKind;
use crate::testing::sync::lock;

impl FakeFileOps {
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

    /// Add a cloud placeholder of `size_bytes`, as iCloud Drive leaves behind.
    ///
    /// Its apparent size is what it would take once downloaded; none of it is
    /// on this disk, and reading it would block on the provider.
    pub fn add_dataless_file(&self, path: impl AsRef<Path>, size_bytes: u64) {
        let path = path.as_ref().to_path_buf();
        self.add_file(&path, &[]);
        self.set_size(&path, size_bytes);
        lock(&self.tree).set_dataless(&path);
    }

    /// Builder form of [`FakeFileOps::add_dataless_file`].
    #[must_use]
    pub fn with_dataless_file(self, path: impl AsRef<Path>, size_bytes: u64) -> Self {
        self.add_dataless_file(path, size_bytes);
        self
    }

    /// Mark an existing directory as a cloud placeholder.
    pub fn add_dataless_dir(&self, path: impl AsRef<Path>) {
        let path = path.as_ref().to_path_buf();
        self.add_dir(&path);
        lock(&self.tree).set_dataless(&path);
    }

    /// Builder form of [`FakeFileOps::add_dataless_dir`].
    #[must_use]
    pub fn with_dataless_dir(self, path: impl AsRef<Path>) -> Self {
        self.add_dataless_dir(path);
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

    /// Pin the allocated size of an existing entry, as a sparse file would report.
    pub fn set_allocated(&self, path: impl AsRef<Path>, allocated_bytes: u64) {
        lock(&self.tree).set_allocated(path.as_ref(), allocated_bytes);
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

    /// A file that occupies exactly `size_bytes`: apparent and allocated sizes
    /// agree, so a test can claim the one number without thinking about blocks.
    #[must_use]
    pub fn with_exact_file(self, path: impl AsRef<Path>, size_bytes: u64) -> Self {
        let path = path.as_ref().to_path_buf();
        let fs = self.with_sized_file(&path, size_bytes);
        fs.set_allocated(&path, size_bytes);
        fs
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
}
