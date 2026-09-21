//! Fakes for the disk, space, and snapshot providers.
//!
//! All three are configurable through a shared reference, so a test can keep the
//! handle returned by [`fake_ports`](super::fake_ports) and still change what the
//! ports answer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::BrozaError;
use crate::model::{Disk, Snapshot, VolumeId};
use crate::ports::{DiskEnumerator, SnapshotProvider, SpaceProvider};

/// Purgeable bytes reported for a mount point nobody configured.
const UNKNOWN_PURGEABLE_BYTES: u64 = 0;

/// A [`DiskEnumerator`] returning a configured list of disks.
#[derive(Debug, Default)]
pub struct FakeDisks {
    /// Disks handed to every caller.
    disks: Mutex<Vec<Disk>>,
}

impl FakeDisks {
    /// An enumerator that reports `disks`.
    pub fn new(disks: Vec<Disk>) -> Self {
        Self { disks: Mutex::new(disks) }
    }

    /// An enumerator that reports nothing.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Replace the disks reported from now on.
    pub fn set_disks(&self, disks: Vec<Disk>) {
        *lock(&self.disks) = disks;
    }
}

impl DiskEnumerator for FakeDisks {
    fn enumerate(&self) -> Result<Vec<Disk>, BrozaError> {
        Ok(lock(&self.disks).clone())
    }
}

/// A [`SpaceProvider`] backed by a mount point to purgeable bytes map.
///
/// A mount point that was not configured reports zero: purgeable space is an
/// estimate, and an unknown estimate is zero rather than a guess (`AGENTS.md` §2.7).
#[derive(Debug, Default)]
pub struct FakeSpace {
    /// Purgeable bytes per mount point.
    purgeable: Mutex<HashMap<PathBuf, u64>>,
}

impl FakeSpace {
    /// A provider that knows nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Report `bytes` as purgeable for `mount_point`.
    pub fn set_purgeable(&self, mount_point: impl AsRef<Path>, bytes: u64) {
        lock(&self.purgeable).insert(mount_point.as_ref().to_path_buf(), bytes);
    }

    /// Builder form of [`FakeSpace::set_purgeable`].
    #[must_use]
    pub fn with_purgeable(self, mount_point: impl AsRef<Path>, bytes: u64) -> Self {
        self.set_purgeable(mount_point, bytes);
        self
    }
}

impl SpaceProvider for FakeSpace {
    fn purgeable_bytes(&self, mount_point: &Path) -> Result<u64, BrozaError> {
        Ok(lock(&self.purgeable).get(mount_point).copied().unwrap_or(UNKNOWN_PURGEABLE_BYTES))
    }
}

/// A [`SnapshotProvider`] backed by a volume to snapshots map.
#[derive(Debug, Default)]
pub struct FakeSnapshots {
    /// Snapshots per volume.
    snapshots: Mutex<HashMap<VolumeId, Vec<Snapshot>>>,
}

impl FakeSnapshots {
    /// A provider with no snapshots at all.
    pub fn new() -> Self {
        Self::default()
    }

    /// Report `snapshots` for `volume`.
    pub fn set_snapshots(&self, volume: VolumeId, snapshots: Vec<Snapshot>) {
        lock(&self.snapshots).insert(volume, snapshots);
    }

    /// Builder form of [`FakeSnapshots::set_snapshots`].
    #[must_use]
    pub fn with_snapshots(self, volume: VolumeId, snapshots: Vec<Snapshot>) -> Self {
        self.set_snapshots(volume, snapshots);
        self
    }
}

impl SnapshotProvider for FakeSnapshots {
    fn list(&self, volume: &VolumeId) -> Result<Vec<Snapshot>, BrozaError> {
        Ok(lock(&self.snapshots).get(volume).cloned().unwrap_or_default())
    }
}

/// Lock a mutex, recovering the value when another test thread poisoned it.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{FakeDisks, FakeSnapshots, FakeSpace, UNKNOWN_PURGEABLE_BYTES};
    use crate::model::{Disk, Snapshot, VolumeId};
    use crate::ports::{DiskEnumerator, SnapshotProvider, SpaceProvider};

    fn volume_id(id: &str) -> VolumeId {
        id.parse().unwrap_or_else(|e| panic!("{id}: {e}"))
    }

    fn disk() -> Disk {
        Disk {
            id: volume_id("disk0"),
            model: "Apple SSD".to_owned(),
            size_bytes: 1_000,
            internal: true,
            containers: Vec::new(),
        }
    }

    #[test]
    fn the_enumerator_returns_the_disks_it_was_given() {
        let disks = FakeDisks::new(vec![disk()]).enumerate().unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(disks, vec![disk()]);
        assert!(FakeDisks::empty().enumerate().unwrap_or_else(|e| panic!("{e}")).is_empty());
    }

    #[test]
    fn the_enumerator_can_be_reconfigured_through_a_shared_reference() {
        let fake = FakeDisks::empty();

        fake.set_disks(vec![disk()]);

        assert_eq!(fake.enumerate().unwrap_or_else(|e| panic!("{e}")).len(), 1);
    }

    #[test]
    fn purgeable_space_is_zero_for_an_unconfigured_mount_point() {
        let space = FakeSpace::new().with_purgeable("/", 500);

        assert_eq!(space.purgeable_bytes(Path::new("/")).unwrap_or_else(|e| panic!("{e}")), 500);
        assert_eq!(
            space.purgeable_bytes(Path::new("/Volumes/Other")).unwrap_or_else(|e| panic!("{e}")),
            UNKNOWN_PURGEABLE_BYTES
        );
    }

    #[test]
    fn snapshots_are_listed_per_volume() {
        let snapshot = Snapshot { name: "com.apple.TimeMachine.x".to_owned(), uuid: None, purgeable: true };
        let provider = FakeSnapshots::new().with_snapshots(volume_id("disk3s5"), vec![snapshot.clone()]);

        assert_eq!(provider.list(&volume_id("disk3s5")).unwrap_or_else(|e| panic!("{e}")), vec![snapshot]);
        assert!(provider.list(&volume_id("disk3s1")).unwrap_or_else(|e| panic!("{e}")).is_empty());
    }
}
