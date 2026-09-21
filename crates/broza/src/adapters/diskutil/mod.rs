//! Disk enumeration through `diskutil … -plist`.
//!
//! Why a process and not `DiskArbitration`: only `diskutil` reports APFS roles,
//! container free space and the seal state, and its `-plist` output can be
//! recorded and replayed in tests
//! (`docs/adr/0002-diskutil-plist-over-diskarbitration.md`).
//!
//! Every command goes through a [`ProcessRunner`], which enforces the timeout
//! and is the seam tests replace with `FakeRunner`. Parsing is split per
//! command into pure functions, and turning the three outputs into the JSON
//! contract is [`assemble`], which runs nothing at all.

mod assemble;
mod parse;
pub mod plist_apfs;
pub mod plist_info;
pub mod plist_list;
pub mod plist_snapshots;
pub mod purpose;
pub mod roles;
pub mod snapshots;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

pub use plist_apfs::{ApfsContainer, ApfsList, ApfsVolume, parse_apfs_list};
pub use plist_info::{DeviceInfo, parse_info};
pub use plist_list::{DiskList, ListApfsVolume, ListDevice, ListPartition, parse_list};
pub use plist_snapshots::parse_snapshots;
pub use purpose::{RoleExplanation, explain_role, purpose_for};
pub use roles::{roles_to_volume_role, volume_role};
pub use snapshots::DiskutilSnapshots;

use crate::BrozaError;
use crate::model::Disk;
use crate::ports::{DiskEnumerator, ProcessRunner, SpaceProvider};

/// Absolute path of `diskutil`; never resolved through `PATH`.
pub const DISKUTIL: &str = "/usr/sbin/diskutil";
/// Budget for one `diskutil` invocation.
pub const DISKUTIL_TIMEOUT: Duration = Duration::from_secs(20);

/// [`DiskEnumerator`] backed by `diskutil`.
///
/// One instance runs four kinds of command: `list`, `apfs list`, and one `info`
/// per physical disk and per `HFS+` partition. Nothing is cached — a scan reads
/// the machine as it is at that moment.
#[derive(Clone)]
pub struct DiskutilEnumerator<R: ProcessRunner> {
    /// Runs `diskutil` with a hard timeout.
    runner: R,
    /// Supplies the purgeable estimate of each container's data volume.
    space: Arc<dyn SpaceProvider>,
}

impl<R: ProcessRunner> std::fmt::Debug for DiskutilEnumerator<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DiskutilEnumerator { .. }")
    }
}

impl<R: ProcessRunner> DiskutilEnumerator<R> {
    /// An enumerator running commands through `runner`.
    pub fn new(runner: R, space: Arc<dyn SpaceProvider>) -> Self {
        Self { runner, space }
    }

    /// Standard output of `diskutil <args…>`, or an error naming the failure.
    ///
    /// A non-zero exit is an error here, not data: `diskutil` prints its
    /// complaint on standard error and an empty plist on standard output, and
    /// parsing that would silently report a machine with no disks.
    fn capture(&self, args: &[&str]) -> Result<Vec<u8>, BrozaError> {
        let output = self.runner.run(DISKUTIL, args, DISKUTIL_TIMEOUT)?;
        if output.success {
            return Ok(output.stdout);
        }
        Err(BrozaError::Other(format!(
            "`diskutil {}` failed{}: {}",
            args.join(" "),
            output.code.map(|code| format!(" with status {code}")).unwrap_or_default(),
            first_line(&output.stderr_text())
        )))
    }

    /// `diskutil info -plist` for every device whose details Broza needs.
    ///
    /// The physical disks answer "what model is this and is it internal", the
    /// `HFS+` partitions answer "how full is it"; neither question has an
    /// answer in `diskutil list` or `diskutil apfs list`.
    fn collect_info(&self, list: &DiskList) -> Result<assemble::InfoByDevice, BrozaError> {
        let mut infos = BTreeMap::new();
        for device in list.physical_devices() {
            let identifiers = std::iter::once(&device.device_identifier)
                .chain(device.hfs_partitions().map(|partition| &partition.device_identifier));
            for identifier in identifiers {
                let raw = self.capture(&["info", "-plist", identifier])?;
                infos.insert(identifier.clone(), parse_info(&raw)?);
            }
        }
        Ok(infos)
    }
}

impl<R: ProcessRunner> DiskEnumerator for DiskutilEnumerator<R> {
    fn enumerate(&self) -> Result<Vec<Disk>, BrozaError> {
        let list = parse_list(&self.capture(&["list", "-plist"])?)?;
        let apfs = parse_apfs_list(&self.capture(&["apfs", "list", "-plist"])?)?;
        let infos = self.collect_info(&list)?;
        Ok(assemble::assemble_disks(&list, &apfs, &infos, self.space.as_ref()))
    }
}

/// The first line of a message, trimmed; the rest is usage text nobody needs.
fn first_line(message: &str) -> String {
    message.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{DISKUTIL, DISKUTIL_TIMEOUT, DiskutilEnumerator, first_line};
    use crate::BrozaError;
    use crate::ports::{DiskEnumerator, ProcessOutput};
    use crate::testing::{FakeRunner, FakeSpace};

    /// A partition map with one disk, no containers and no `HFS+` partitions.
    const EMPTY_LIST: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>AllDisksAndPartitions</key>
  <array>
    <dict>
      <key>Content</key><string>GUID_partition_scheme</string>
      <key>DeviceIdentifier</key><string>disk0</string>
      <key>Size</key><integer>1000</integer>
    </dict>
  </array>
</dict>
</plist>"#;
    /// An `apfs list` output without a single container.
    const NO_CONTAINERS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Containers</key><array/></dict></plist>"#;
    /// A `diskutil info` output for `disk0`.
    const DISK0_INFO: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>DeviceIdentifier</key><string>disk0</string>
  <key>Internal</key><true/>
  <key>MediaName</key><string>APPLE SSD</string>
  <key>TotalSize</key><integer>1000</integer>
</dict>
</plist>"#;

    fn ok(stdout: &[u8]) -> ProcessOutput {
        ProcessOutput { success: true, code: Some(0), stdout: stdout.to_vec(), stderr: Vec::new() }
    }

    fn enumerator(runner: FakeRunner) -> DiskutilEnumerator<Arc<FakeRunner>> {
        DiskutilEnumerator::new(Arc::new(runner), Arc::new(FakeSpace::new()))
    }

    fn scripted() -> FakeRunner {
        FakeRunner::new()
            .with_output(DISKUTIL, &["list", "-plist"], ok(EMPTY_LIST))
            .with_output(DISKUTIL, &["apfs", "list", "-plist"], ok(NO_CONTAINERS))
            .with_output(DISKUTIL, &["info", "-plist", "disk0"], ok(DISK0_INFO))
    }

    #[test]
    fn every_command_is_the_absolute_path_with_the_agreed_timeout() {
        let enumerator = enumerator(scripted());

        let disks = enumerator.enumerate().unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(disks.len(), 1);
        let calls = enumerator.runner.calls();
        assert_eq!(calls.len(), 3, "list, apfs list and one info");
        assert!(calls.iter().all(|call| call.program == DISKUTIL));
        assert!(calls.iter().all(|call| call.timeout == DISKUTIL_TIMEOUT));
        assert_eq!(calls[0].args, vec!["list".to_owned(), "-plist".to_owned()]);
    }

    #[test]
    fn a_diskutil_that_exits_non_zero_is_an_error_and_not_an_empty_machine() {
        let runner = FakeRunner::new().with_output(
            DISKUTIL,
            &["list", "-plist"],
            ProcessOutput {
                success: false,
                code: Some(1),
                stdout: Vec::new(),
                stderr: b"Unable to run because unable to use the DiskManagement framework\n".to_vec(),
            },
        );

        let err = enumerator(runner).enumerate().err();

        let Some(BrozaError::Other(message)) = err else { panic!("expected BrozaError::Other") };
        assert!(message.contains("diskutil list -plist"), "{message}");
        assert!(message.contains("status 1"), "{message}");
        assert!(message.contains("DiskManagement"), "{message}");
    }

    #[test]
    fn a_diskutil_that_cannot_be_run_at_all_is_reported_as_it_is() {
        let runner = FakeRunner::new().with_failure(DISKUTIL, &["list", "-plist"], "timed out");

        let err = enumerator(runner).enumerate().err();

        assert!(matches!(err, Some(BrozaError::Other(message)) if message == "timed out"));
    }

    #[test]
    fn output_that_is_not_a_plist_names_the_command_that_produced_it() {
        let runner = FakeRunner::new().with_output(DISKUTIL, &["list", "-plist"], ok(b"<broken"));

        let err = enumerator(runner).enumerate().err();

        let Some(BrozaError::Other(message)) = err else { panic!("expected BrozaError::Other") };
        assert!(message.contains("diskutil list -plist"), "{message}");
    }

    #[test]
    fn a_failing_info_stops_the_enumeration_rather_than_inventing_a_disk() {
        let runner = FakeRunner::new()
            .with_output(DISKUTIL, &["list", "-plist"], ok(EMPTY_LIST))
            .with_output(DISKUTIL, &["apfs", "list", "-plist"], ok(NO_CONTAINERS));

        let err = enumerator(runner).enumerate().err();

        assert!(err.is_some(), "an info Broza cannot read must not become a disk with no model");
    }

    #[test]
    fn only_the_first_line_of_a_complaint_is_reported() {
        assert_eq!(first_line("\n  boom  \nusage: diskutil\n"), "boom");
        assert_eq!(first_line(""), "");
    }
}
