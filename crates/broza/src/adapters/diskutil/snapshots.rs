//! Listing APFS local snapshots through `diskutil apfs listSnapshots -plist`.
//!
//! `tmutil listlocalsnapshots` lists the same snapshots by name, without the
//! `Purgeable` flag that decides whether a snapshot is actionable at all. It is
//! therefore not a fallback here: when `diskutil` fails, Broza reports the
//! failure instead of returning a list it cannot reason about. Deletion goes
//! through `tmutil deletelocalsnapshots <date>`, the only supported tool for
//! it, behind an `Approved<SnapshotDelete>` token
//! (`docs/adr/0002-diskutil-plist-over-diskarbitration.md`).

use std::sync::Arc;

use crate::BrozaError;
use crate::adapters::diskutil::{DISKUTIL, DISKUTIL_TIMEOUT, first_line, parse_snapshots};
use crate::model::{Snapshot, TIME_MACHINE_PREFIX, VolumeId};
use crate::ports::{ProcessRunner, SnapshotProvider};
use crate::safety::guard::{Approved, ApprovedItem, SnapshotDelete};

/// Time Machine's own tool, the only one that deletes local snapshots.
pub const TMUTIL: &str = "/usr/bin/tmutil";
/// `tmutil deletelocalsnapshots` can take a while on a busy volume.
const TMUTIL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

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
        let Some(name) = item.snapshot().filter(|_| covered) else {
            return Err(BrozaError::Other(format!(
                "snapshot deletion: `{}` is not one of the snapshots the guard approved",
                item.path().display()
            )));
        };
        let date = date_stamp(name).ok_or_else(|| {
            BrozaError::Other(format!("snapshot `{name}` is not a Time Machine local snapshot"))
        })?;
        let args = ["deletelocalsnapshots", date];
        let output = self.runner.run(TMUTIL, &args, TMUTIL_TIMEOUT)?;
        if output.success {
            return Ok(());
        }
        let reason = first_line(&output.stderr_text());
        if is_privilege_refusal(&reason) {
            return Err(BrozaError::PermissionDenied { path: std::path::PathBuf::from(TMUTIL) });
        }
        Err(BrozaError::Other(format!("tmutil deletelocalsnapshots {date} failed: {reason}")))
    }
}

/// The `YYYY-MM-DD-HHMMSS` stamp `tmutil` takes, from a Time Machine snapshot name.
fn date_stamp(name: &str) -> Option<&str> {
    name.strip_prefix(TIME_MACHINE_PREFIX).and_then(|rest| rest.strip_suffix(".local"))
}

/// `tmutil`'s wording when it wants root.
fn is_privilege_refusal(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("not permitted") || lower.contains("permission denied") || lower.contains("root")
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
