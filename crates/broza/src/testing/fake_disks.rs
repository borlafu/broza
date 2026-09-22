//! Fakes for the disk, space, and snapshot providers.
//!
//! All three are configurable through a shared reference, so a test can keep the
//! handle returned by [`fake_ports`](super::fake_ports) and still change what the
//! ports answer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::BrozaError;
use crate::model::{Disk, Snapshot, VolumeId, Warning};
use crate::ports::{DiskEnumerator, EnumerationReport, SnapshotProvider, SpaceProvider};
use crate::safety::guard::{Approved, ApprovedItem, SnapshotDelete};
use crate::testing::sync::lock;

/// Purgeable bytes reported for a mount point nobody configured.
const UNKNOWN_PURGEABLE_BYTES: u64 = 0;

/// A [`DiskEnumerator`] returning a configured report.
#[derive(Debug, Default)]
pub struct FakeDisks {
    /// Report handed to every caller.
    report: Mutex<EnumerationReport>,
}

impl FakeDisks {
    /// An enumerator that reports `disks` and no warnings.
    pub fn new(disks: Vec<Disk>) -> Self {
        Self { report: Mutex::new(EnumerationReport { disks, warnings: Vec::new() }) }
    }

    /// An enumerator that reports nothing.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Replace the disks reported from now on, keeping the warnings.
    pub fn set_disks(&self, disks: Vec<Disk>) {
        lock(&self.report).disks = disks;
    }

    /// Replace the warnings reported from now on, keeping the disks.
    pub fn set_warnings(&self, warnings: Vec<Warning>) {
        lock(&self.report).warnings = warnings;
    }
}

impl DiskEnumerator for FakeDisks {
    fn enumerate(&self) -> Result<EnumerationReport, BrozaError> {
        Ok(lock(&self.report).clone())
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
    /// Names deleted so far, in order.
    deleted: Mutex<Vec<String>>,
    /// When set, every deletion is refused as if `tmutil` wanted root.
    refuse_deletions: Mutex<bool>,
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

impl FakeSnapshots {
    /// The snapshot names deleted so far, in order.
    pub fn deleted(&self) -> Vec<String> {
        lock(&self.deleted).clone()
    }

    /// Make every deletion fail with a privilege refusal.
    pub fn refuse_deletions(&self) {
        *lock(&self.refuse_deletions) = true;
    }
}

impl SnapshotProvider for FakeSnapshots {
    fn list(&self, volume: &VolumeId) -> Result<Vec<Snapshot>, BrozaError> {
        Ok(lock(&self.snapshots).get(volume).cloned().unwrap_or_default())
    }

    fn delete(&self, token: &Approved<SnapshotDelete>, item: &ApprovedItem) -> Result<(), BrozaError> {
        let covered = token.items().iter().any(|approved| approved == item);
        let Some(name) = item.snapshot().filter(|_| covered) else {
            return Err(BrozaError::Other(format!(
                "snapshot deletion: `{}` is not one of the snapshots the guard approved",
                item.path().display()
            )));
        };
        if *lock(&self.refuse_deletions) {
            return Err(BrozaError::PermissionDenied { path: std::path::PathBuf::from("/usr/bin/tmutil") });
        }
        for snapshots in lock(&self.snapshots).values_mut() {
            snapshots.retain(|snapshot| snapshot.name != name);
        }
        lock(&self.deleted).push(name.to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{FakeDisks, FakeSnapshots, FakeSpace, UNKNOWN_PURGEABLE_BYTES};
    use crate::model::{Disk, Snapshot, VolumeId, Warning};
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
        let report = FakeDisks::new(vec![disk()]).enumerate().unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(report.disks, vec![disk()]);
        assert!(report.warnings.is_empty());
        let empty = FakeDisks::empty().enumerate().unwrap_or_else(|e| panic!("{e}"));
        assert!(empty.disks.is_empty());
    }

    #[test]
    fn the_enumerator_can_be_reconfigured_through_a_shared_reference() {
        let fake = FakeDisks::empty();
        let warning = Warning { code: "x".to_owned(), message: "y".to_owned(), path: None };

        fake.set_disks(vec![disk()]);
        fake.set_warnings(vec![warning.clone()]);

        let report = fake.enumerate().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(report.disks.len(), 1);
        assert_eq!(report.warnings, vec![warning]);
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
        let snapshot = Snapshot {
            name: "com.apple.TimeMachine.x".to_owned(),
            uuid: None,
            purgeable: true,
            volume: None,
            mount_point: None,
        };
        let provider = FakeSnapshots::new().with_snapshots(volume_id("disk3s5"), vec![snapshot.clone()]);

        assert_eq!(provider.list(&volume_id("disk3s5")).unwrap_or_else(|e| panic!("{e}")), vec![snapshot]);
        assert!(provider.list(&volume_id("disk3s1")).unwrap_or_else(|e| panic!("{e}")).is_empty());
    }
}
