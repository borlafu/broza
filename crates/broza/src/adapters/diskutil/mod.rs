//! Disk enumeration through `diskutil … -plist`.
//!
//! Why a process and not `DiskArbitration`: only `diskutil` reports APFS roles,
//! container free space and the seal state, and its `-plist` output can be
//! recorded and replayed in tests
//! (`docs/adr/0002-diskutil-plist-over-diskarbitration.md`).
//!
//! Every command goes through a [`ProcessRunner`], which enforces the timeout
//! and is the seam tests replace with `FakeRunner`. Parsing is split per
//! command into pure functions, and turning the outputs into the JSON contract
//! is [`assemble`], which runs nothing at all.

mod assemble;
mod budget;
mod devices;
mod hfs;
mod inputs;
mod parse;
pub mod plist_apfs;
pub mod plist_info;
pub mod plist_list;
pub mod plist_snapshots;
pub mod purpose;
pub mod roles;
pub mod snapshots;
#[cfg(test)]
mod tests_support;
mod volumes;

use std::sync::Arc;
use std::time::Duration;

pub use plist_apfs::{ApfsContainer, ApfsList, ApfsVolume, parse_apfs_list};
pub use plist_info::{DeviceInfo, parse_info};
pub use plist_list::{DiskList, ListApfsVolume, ListDevice, ListPartition, parse_list};
pub use plist_snapshots::parse_snapshots;
pub use purpose::{RoleExplanation, explain_role, purpose_for, purpose_for_token, purpose_for_volume};
pub use roles::{VolumeFacts, roles_to_volume_role, volume_role};
pub use snapshots::DiskutilSnapshots;

use crate::BrozaError;
use crate::ports::{DiskEnumerator, EnumerationReport, FileOps, ProcessRunner, SpaceProvider};
use budget::Budget;
use inputs::{InfoByDevice, Inputs};

/// Absolute path of `diskutil`; never resolved through `PATH`.
pub const DISKUTIL: &str = "/usr/sbin/diskutil";
/// Budget for one `diskutil` invocation.
pub const DISKUTIL_TIMEOUT: Duration = Duration::from_secs(20);
/// Budget for a whole enumeration, however many commands it takes.
///
/// A machine with a dozen mounted disk images issues a dozen `info` calls, and
/// a per-command timeout alone would let a slow machine spend minutes before
/// the caller hears anything. This bounds the lot.
pub const ENUMERATION_BUDGET: Duration = Duration::from_secs(60);

/// [`DiskEnumerator`] backed by `diskutil`.
///
/// One run issues `list`, `apfs list`, one `info` per physical disk and per
/// `HFS+` partition, and one more for each APFS volume that declares no role —
/// the only volumes whose writability has to be checked before they can be
/// called the user's. Nothing is cached: a scan reads the machine as it is.
#[derive(Clone)]
pub struct DiskutilEnumerator {
    /// Runs `diskutil` with a hard timeout.
    runner: Arc<dyn ProcessRunner>,
    /// Supplies the purgeable estimate of each container's data volume.
    space: Arc<dyn SpaceProvider>,
    /// Used only to look for a Time Machine marker at a volume root.
    fs: Arc<dyn FileOps>,
}

impl std::fmt::Debug for DiskutilEnumerator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DiskutilEnumerator { .. }")
    }
}

impl DiskutilEnumerator {
    /// An enumerator running commands through `runner`.
    pub fn new(runner: Arc<dyn ProcessRunner>, space: Arc<dyn SpaceProvider>, fs: Arc<dyn FileOps>) -> Self {
        Self { runner, space, fs }
    }

    /// Standard output of `diskutil <args…>`, or an error naming the failure.
    ///
    /// A non-zero exit is an error here, not data: `diskutil` prints its
    /// complaint on standard error and an empty plist on standard output, and
    /// parsing that would silently report a machine with no disks.
    fn capture(&self, args: &[&str], budget: &Budget) -> Result<Vec<u8>, BrozaError> {
        let output = self.runner.run(DISKUTIL, args, budget.next_timeout(DISKUTIL_TIMEOUT)?)?;
        if output.success {
            return Ok(output.stdout);
        }
        Err(classify(args, output.code, &first_line(&output.stderr_text())))
    }

    /// `diskutil info -plist` for every device whose details Broza needs.
    ///
    /// Physical disks answer "what model is this and is it internal", `HFS+`
    /// partitions answer "how full is it", and roleless APFS volumes answer
    /// "may anyone write here" — the question that decides whether they are
    /// the user's disk or something Broza must leave alone (`AGENTS.md` §2.3).
    fn collect_info(
        &self,
        list: &DiskList,
        apfs: &ApfsList,
        budget: &Budget,
    ) -> Result<InfoByDevice, BrozaError> {
        let mut infos = InfoByDevice::new();
        for identifier in devices_to_inspect(list, apfs) {
            let raw = self.capture(&["info", "-plist", &identifier], budget)?;
            infos.insert(identifier, parse_info(&raw)?);
        }
        Ok(infos)
    }
}

impl DiskEnumerator for DiskutilEnumerator {
    fn enumerate(&self) -> Result<EnumerationReport, BrozaError> {
        let budget = Budget::start(ENUMERATION_BUDGET);
        let list = parse_list(&self.capture(&["list", "-plist"], &budget)?)?;
        let apfs = parse_apfs_list(&self.capture(&["apfs", "list", "-plist"], &budget)?)?;
        let infos = self.collect_info(&list, &apfs, &budget)?;
        let inputs = Inputs {
            list: &list,
            apfs: &apfs,
            infos: &infos,
            space: self.space.as_ref(),
            fs: self.fs.as_ref(),
        };
        let mut warnings = Vec::new();
        let disks = assemble::assemble_disks(&inputs, &mut warnings);
        Ok(EnumerationReport { disks, warnings })
    }
}

/// Every device `diskutil info` has to be run for, in a stable order.
fn devices_to_inspect(list: &DiskList, apfs: &ApfsList) -> Vec<String> {
    let physical = list.physical_devices().flat_map(|device| {
        std::iter::once(device.device_identifier.clone())
            .chain(device.hfs_partitions().map(|partition| partition.device_identifier.clone()))
    });
    let roleless = apfs
        .containers
        .iter()
        .flat_map(|container| &container.volumes)
        .filter(|volume| volume.roles.is_empty())
        .filter(|volume| {
            list.apfs_volume(&volume.device_identifier).is_some_and(|listed| listed.mount_point.is_some())
        })
        .map(|volume| volume.device_identifier.clone());
    let mut devices: Vec<String> = physical.chain(roleless).collect();
    devices.dedup();
    devices
}

/// Turn a refusal from `diskutil` into the error that describes it.
///
/// The exit code alone is useless — `diskutil` uses `1` for everything — so the
/// message decides. A target that is not there is `TARGET_NOT_FOUND` (exit 4)
/// and a refusal is `PERMISSION_DENIED` (exit 3); anything else keeps the
/// text and exits `1` (`docs/cli-spec.md` §2).
pub(crate) fn classify(args: &[&str], code: Option<i32>, reason: &str) -> BrozaError {
    let command = format!("diskutil {}", args.join(" "));
    let lowered = reason.to_lowercase();
    if ["could not find", "unable to find", "no such file", "does not exist"]
        .iter()
        .any(|hint| lowered.contains(hint))
    {
        return BrozaError::TargetNotFound(format!("`{command}`: {reason}"));
    }
    if ["permission", "not permitted", "authorization", "privileges"]
        .iter()
        .any(|hint| lowered.contains(hint))
    {
        return BrozaError::PermissionDenied { path: std::path::PathBuf::from(DISKUTIL) };
    }
    let status = code.map(|code| format!(" with status {code}")).unwrap_or_default();
    BrozaError::Other(format!("`{command}` failed{status}: {reason}"))
}

/// The first line of a message, trimmed; the rest is usage text nobody needs.
pub(crate) fn first_line(message: &str) -> String {
    message.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{DISKUTIL, DISKUTIL_TIMEOUT, DiskutilEnumerator, classify, first_line};
    use crate::BrozaError;
    use crate::ports::{DiskEnumerator, FileOps, ProcessOutput, ProcessRunner, SpaceProvider};
    use crate::testing::{FakeFileOps, FakeRunner, FakeSpace};

    /// A partition map with one physical disk and one container device.
    const LIST: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
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

    fn failure(code: i32, stderr: &str) -> ProcessOutput {
        ProcessOutput {
            success: false,
            code: Some(code),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    fn enumerator(runner: &Arc<FakeRunner>) -> DiskutilEnumerator {
        DiskutilEnumerator::new(
            Arc::clone(runner) as Arc<dyn ProcessRunner>,
            Arc::new(FakeSpace::new()) as Arc<dyn SpaceProvider>,
            Arc::new(FakeFileOps::new()) as Arc<dyn FileOps>,
        )
    }

    fn scripted() -> Arc<FakeRunner> {
        Arc::new(
            FakeRunner::new()
                .with_output(DISKUTIL, &["list", "-plist"], ok(LIST))
                .with_output(DISKUTIL, &["apfs", "list", "-plist"], ok(NO_CONTAINERS))
                .with_output(DISKUTIL, &["info", "-plist", "disk0"], ok(DISK0_INFO)),
        )
    }

    #[test]
    fn every_command_is_the_absolute_path_within_the_enumeration_budget() {
        let runner = scripted();

        let report = enumerator(&runner).enumerate().unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(report.disks.len(), 1);
        assert!(report.warnings.is_empty());
        let calls = runner.calls();
        assert_eq!(calls.len(), 3, "list, apfs list and one info");
        assert!(calls.iter().all(|call| call.program == DISKUTIL));
        assert!(calls.iter().all(|call| call.timeout <= DISKUTIL_TIMEOUT));
        assert!(calls.iter().all(|call| !call.timeout.is_zero()));
        assert_eq!(calls[0].args, vec!["list".to_owned(), "-plist".to_owned()]);
    }

    #[test]
    fn a_diskutil_that_exits_non_zero_is_an_error_and_not_an_empty_machine() {
        let runner = Arc::new(FakeRunner::new().with_output(
            DISKUTIL,
            &["list", "-plist"],
            failure(1, "Unable to run because unable to use the DiskManagement framework\n"),
        ));

        let err = enumerator(&runner).enumerate().err();

        let Some(BrozaError::Other(message)) = err else { panic!("expected BrozaError::Other") };
        assert!(message.contains("diskutil list -plist"), "{message}");
        assert!(message.contains("status 1"), "{message}");
        assert!(message.contains("DiskManagement"), "{message}");
    }

    #[test]
    fn a_target_diskutil_cannot_find_is_a_not_found_error() {
        let err = classify(&["info", "-plist", "disk9"], Some(1), "Could not find disk: disk9");

        assert!(matches!(err, BrozaError::TargetNotFound(message) if message.contains("disk9")));
    }

    #[test]
    fn a_refusal_is_a_permission_error_whatever_the_wording() {
        for reason in [
            "Permission denied",
            "Operation not permitted",
            "You must have authorization to do that",
            "insufficient privileges",
        ] {
            let err = classify(&["list", "-plist"], Some(1), reason);

            assert!(matches!(err, BrozaError::PermissionDenied { .. }), "{reason}: {err:?}");
        }
    }

    #[test]
    fn anything_else_keeps_the_text_diskutil_printed() {
        let err = classify(&["list", "-plist"], None, "something new");

        let BrozaError::Other(message) = err else { panic!("expected BrozaError::Other") };
        assert!(message.contains("something new"), "{message}");
        assert!(!message.contains("status"), "no code, no status: {message}");
    }

    #[test]
    fn a_diskutil_that_cannot_be_run_at_all_is_reported_as_it_is() {
        let runner = Arc::new(FakeRunner::new().with_failure(DISKUTIL, &["list", "-plist"], "timed out"));

        let err = enumerator(&runner).enumerate().err();

        assert!(matches!(err, Some(BrozaError::Other(message)) if message == "timed out"));
    }

    #[test]
    fn output_that_is_not_a_plist_names_the_command_that_produced_it() {
        let runner = Arc::new(FakeRunner::new().with_output(DISKUTIL, &["list", "-plist"], ok(b"<broken")));

        let err = enumerator(&runner).enumerate().err();

        let Some(BrozaError::Other(message)) = err else { panic!("expected BrozaError::Other") };
        assert!(message.contains("diskutil list -plist"), "{message}");
    }

    #[test]
    fn a_failing_info_stops_the_enumeration_rather_than_inventing_a_disk() {
        let runner =
            Arc::new(FakeRunner::new().with_output(DISKUTIL, &["list", "-plist"], ok(LIST)).with_output(
                DISKUTIL,
                &["apfs", "list", "-plist"],
                ok(NO_CONTAINERS),
            ));

        let err = enumerator(&runner).enumerate().err();

        assert!(err.is_some(), "an info Broza cannot read must not become a disk with no model");
    }

    #[test]
    fn only_the_first_line_of_a_complaint_is_reported() {
        assert_eq!(first_line("\n  boom  \nusage: diskutil\n"), "boom");
        assert_eq!(first_line(""), "");
    }
}
