//! Disk, space, and snapshot providers.

use std::path::Path;

use crate::BrozaError;
use crate::model::{Disk, Snapshot, VolumeId};

/// Enumerates physical disks with their containers and volumes.
pub trait DiskEnumerator: Send + Sync {
    /// All disks visible to the system, internal and external.
    fn enumerate(&self) -> Result<Vec<Disk>, BrozaError>;
}

/// Reports purgeable space for a mounted volume.
pub trait SpaceProvider: Send + Sync {
    /// Bytes macOS may reclaim on its own for the volume mounted at `mount_point`.
    /// An estimate; never added to free space.
    fn purgeable_bytes(&self, mount_point: &Path) -> Result<u64, BrozaError>;
}

/// Lists APFS local snapshots. Deletion is added in the safety kernel milestone and
/// requires an `Approved` token.
pub trait SnapshotProvider: Send + Sync {
    /// Snapshots of `volume`.
    fn list(&self, volume: &VolumeId) -> Result<Vec<Snapshot>, BrozaError>;
}
