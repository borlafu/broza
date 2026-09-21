//! Listing APFS local snapshots through `diskutil apfs listSnapshots -plist`.
//!
//! `tmutil listlocalsnapshots` lists the same snapshots by name, without the
//! `Purgeable` flag that decides whether a snapshot is actionable at all. It is
//! therefore not a fallback here: when `diskutil` fails, Broza reports the
//! failure instead of returning a list it cannot reason about. `tmutil` enters
//! the picture in M4, for deletion
//! (`docs/adr/0002-diskutil-plist-over-diskarbitration.md`).

use std::sync::Arc;

use crate::BrozaError;
use crate::adapters::diskutil::{DISKUTIL, DISKUTIL_TIMEOUT, first_line, parse_snapshots};
use crate::model::{Snapshot, VolumeId};
use crate::ports::{ProcessRunner, SnapshotProvider};

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
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::DiskutilSnapshots;
    use crate::BrozaError;
    use crate::adapters::diskutil::{DISKUTIL, DISKUTIL_TIMEOUT};
    use crate::model::VolumeId;
    use crate::ports::{ProcessOutput, ProcessRunner, SnapshotProvider};
    use crate::testing::FakeRunner;

    /// One purgeable snapshot, as `diskutil` writes it.
    const ONE_SNAPSHOT: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>Snapshots</key>
  <array>
    <dict>
      <key>Purgeable</key><true/>
      <key>SnapshotName</key><string>com.apple.TimeMachine.2026-09-20-101530.local</string>
      <key>SnapshotUUID</key><string>00000001-1111-4222-8333-000000000001</string>
    </dict>
  </array>
</dict>
</plist>"#;

    fn volume() -> VolumeId {
        "disk3s5".parse().unwrap_or_else(|e| panic!("{e}"))
    }

    fn args() -> [&'static str; 4] {
        ["apfs", "listSnapshots", "-plist", "disk3s5"]
    }

    #[test]
    fn the_snapshots_of_a_volume_are_listed_with_their_purgeable_flag() {
        let runner = Arc::new(FakeRunner::new().with_output(
            DISKUTIL,
            &args(),
            ProcessOutput { success: true, code: Some(0), stdout: ONE_SNAPSHOT.to_vec(), stderr: Vec::new() },
        ));

        let listed = DiskutilSnapshots::new(Arc::clone(&runner) as Arc<dyn ProcessRunner>)
            .list(&volume())
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(listed.len(), 1);
        assert!(listed[0].purgeable);
        assert_eq!(listed[0].name, "com.apple.TimeMachine.2026-09-20-101530.local");
        assert_eq!(runner.calls()[0].timeout, DISKUTIL_TIMEOUT);
    }

    #[test]
    fn a_failing_command_is_an_error_and_never_falls_back_to_tmutil() {
        let runner = Arc::new(FakeRunner::new().with_output(
            DISKUTIL,
            &args(),
            ProcessOutput {
                success: false,
                code: Some(1),
                stdout: Vec::new(),
                stderr: b"Could not find disk: disk3s5\n".to_vec(),
            },
        ));

        let err = DiskutilSnapshots::new(Arc::clone(&runner) as Arc<dyn ProcessRunner>).list(&volume()).err();

        let Some(BrozaError::TargetNotFound(message)) = err else {
            panic!("a volume diskutil cannot find is a missing target, got {err:?}")
        };
        assert!(message.contains("Could not find disk"), "{message}");
        assert_eq!(runner.calls().len(), 1, "no second command is attempted");
    }

    #[test]
    fn a_command_that_cannot_be_run_keeps_its_own_error() {
        let runner = FakeRunner::new().with_failure(DISKUTIL, &args(), "timed out");

        let err = DiskutilSnapshots::new(Arc::new(runner) as Arc<dyn ProcessRunner>).list(&volume()).err();

        assert!(matches!(err, Some(BrozaError::Other(message)) if message == "timed out"));
    }

    #[test]
    fn output_that_is_not_a_plist_is_reported_as_a_parse_failure() {
        let runner = FakeRunner::new().with_output(
            DISKUTIL,
            &args(),
            ProcessOutput { success: true, code: Some(0), stdout: b"nope".to_vec(), stderr: Vec::new() },
        );

        let err = DiskutilSnapshots::new(Arc::new(runner) as Arc<dyn ProcessRunner>).list(&volume()).err();

        let Some(BrozaError::Other(message)) = err else { panic!("expected BrozaError::Other") };
        assert!(message.contains("listSnapshots"), "{message}");
    }
}
