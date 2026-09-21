//! The exclusive-operation knobs of [`FakeFileOps`](super::FakeFileOps).
//!
//! What a test needs to reproduce a filesystem without `renamex_np` (exFAT) and
//! to observe which paths a live [`FsLock`](crate::ports::FsLock) guard holds.

use std::path::Path;

use crate::ports::RenameMode;
use crate::testing::fake_fs::FakeFileOps;
use crate::testing::sync::lock;

impl FakeFileOps {
    /// Make everything under `prefix` answer `ENOTSUP` to an exclusive rename.
    ///
    /// What exFAT and several network filesystems do: the rename still happens,
    /// but only after a separate existence check, and the caller is told so
    /// with [`RenameMode::CheckedFallback`].
    pub fn deny_exclusive_rename(&self, prefix: impl AsRef<Path>) {
        lock(&self.no_exclusive_rename).push(prefix.as_ref().to_path_buf());
    }

    /// Builder form of [`FakeFileOps::deny_exclusive_rename`].
    #[must_use]
    pub fn with_denied_exclusive_rename(self, prefix: impl AsRef<Path>) -> Self {
        self.deny_exclusive_rename(prefix);
        self
    }

    /// `true` when `path` is currently locked by a live guard.
    pub fn is_locked(&self, path: impl AsRef<Path>) -> bool {
        lock(&self.locks).contains(path.as_ref())
    }

    /// Which kind of exclusive rename this destination supports.
    pub(super) fn rename_mode_for(&self, destination: &Path) -> RenameMode {
        if lock(&self.no_exclusive_rename).iter().any(|prefix| destination.starts_with(prefix)) {
            return RenameMode::CheckedFallback;
        }
        RenameMode::Exclusive
    }
}
