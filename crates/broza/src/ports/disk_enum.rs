//! Disk, space, and snapshot providers.

use std::path::Path;

use crate::BrozaError;
use crate::model::{Disk, Snapshot, VolumeId, Warning};
use crate::safety::guard::{Approved, ApprovedItem, SnapshotDelete};

/// What one enumeration found, and what it could not make sense of.
///
/// An enumeration either fails outright — no `diskutil`, no permission — or it
/// returns the machine it could describe. Everything in between (a container
/// with an unreadable identifier, a capacity that is not a number) becomes a
/// warning rather than an error, because a partial map of the machine still
/// protects the volumes it does know about (`AGENTS.md` §6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnumerationReport {
    /// Physical disks, with their containers and volumes.
    pub disks: Vec<Disk>,
    /// Everything the enumerator had to skip or correct, for `warnings[]`.
    pub warnings: Vec<Warning>,
}

/// Enumerates physical disks with their containers and volumes.
pub trait DiskEnumerator: Send + Sync {
    /// All disks visible to the system, internal and external.
    fn enumerate(&self) -> Result<EnumerationReport, BrozaError>;
}

/// Reports purgeable space for a mounted volume.
pub trait SpaceProvider: Send + Sync {
    /// Bytes macOS may reclaim on its own for the volume mounted at `mount_point`.
    /// An estimate; never added to free space.
    fn purgeable_bytes(&self, mount_point: &Path) -> Result<u64, BrozaError>;
}

/// Lists and deletes APFS local snapshots.
pub trait SnapshotProvider: Send + Sync {
    /// Snapshots of `volume`.
    fn list(&self, volume: &VolumeId) -> Result<Vec<Snapshot>, BrozaError>;

    /// Delete the snapshot `item` names, which must be one of `token`'s items.
    ///
    /// # Errors
    ///
    /// [`BrozaError::Other`] when the token does not cover `item`;
    /// [`BrozaError::PermissionDenied`] when the tool refuses for lack of
    /// privileges (Broza never escalates); whatever else the tool reports.
    fn delete(&self, token: &Approved<SnapshotDelete>, item: &ApprovedItem) -> Result<(), BrozaError>;
}
