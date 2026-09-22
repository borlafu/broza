//! Listing APFS local snapshots through `diskutil apfs listSnapshots -plist`.
//!
//! `tmutil listlocalsnapshots` lists the same snapshots by name, without the
//! `Purgeable` flag that decides whether a snapshot is actionable at all. It is
//! therefore not a fallback here: when `diskutil` fails, Broza reports the
//! failure instead of returning a list it cannot reason about. Deletion goes
//! through `diskutil apfs deleteSnapshot <volume> -uuid <uuid>`, which names one
//! snapshot on one volume — `tmutil deletelocalsnapshots <date>` would remove
//! every snapshot of that date on every volume — behind an
//! `Approved<SnapshotDelete>` token (`docs/adr/0007-snapshot-deletion-by-uuid.md`).

use std::sync::Arc;

use crate::BrozaError;
use crate::adapters::diskutil::{DISKUTIL, DISKUTIL_TIMEOUT, first_line, parse_snapshots};
use crate::model::{Snapshot, VolumeId, is_uuid};
use crate::ports::{ProcessRunner, SnapshotProvider};
use crate::safety::guard::{Approved, ApprovedItem, SnapshotDelete};

/// Deleting a snapshot returns once APFS has queued the reclaim; the bound of
/// every other `diskutil` call is generous for that.
const DELETE_SNAPSHOT_TIMEOUT: std::time::Duration = DISKUTIL_TIMEOUT;

/// [`SnapshotProvider`] backed by `diskutil`.
#[derive(Clone)]
pub struct DiskutilSnapshots {
    /// Runs `diskutil` with a hard timeout.
    runner: Arc<dyn ProcessRunner>,
}

impl std::fmt::Debug for DiskutilSnapshots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DiskutilSnapshots { .. }")
    }
}

impl DiskutilSnapshots {
    /// A provider running commands through `runner`.
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self { runner }
    }
}

impl SnapshotProvider for DiskutilSnapshots {
    fn list(&self, volume: &VolumeId) -> Result<Vec<Snapshot>, BrozaError> {
        let args = ["apfs", "listSnapshots", "-plist", volume.as_str()];
        let output = self.runner.run(DISKUTIL, &args, DISKUTIL_TIMEOUT)?;
        if !output.success {
            return Err(crate::adapters::diskutil::classify(
                &args,
                output.code,
                &first_line(&output.stderr_text()),
            ));
        }
        parse_snapshots(&output.stdout)
    }

    fn delete(&self, token: &Approved<SnapshotDelete>, item: &ApprovedItem) -> Result<(), BrozaError> {
        let covered = token.items().iter().any(|approved| approved == item);
        let Some(snapshot) = item.snapshot().filter(|_| covered) else {
            return Err(BrozaError::Other(format!(
                "snapshot deletion: `{}` is not one of the snapshots the guard approved",
                item.path().display()
            )));
        };
        if !is_uuid(&snapshot.uuid) {
            return Err(BrozaError::Other(format!("snapshot `{}` has no usable UUID", snapshot.name)));
        }
        let args = ["apfs", "deleteSnapshot", snapshot.volume.as_str(), "-uuid", snapshot.uuid.as_str()];
        let output = self.runner.run(DISKUTIL, &args, DELETE_SNAPSHOT_TIMEOUT)?;
        if output.success {
            return Ok(());
        }
        let reason = first_line(&output.stderr_text());
        if is_privilege_refusal(&reason) {
            return Err(BrozaError::PermissionDenied { path: std::path::PathBuf::from(DISKUTIL) });
        }
        Err(BrozaError::Other(format!(
            "diskutil apfs deleteSnapshot {} -uuid {} failed: {reason}",
            snapshot.volume, snapshot.uuid
        )))
    }
}

/// `diskutil`'s wording when it wants ownership of the disk or root.
fn is_privilege_refusal(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("not permitted")
        || lower.contains("permission denied")
        || lower.contains("ownership")
        || lower.contains("root")
}
