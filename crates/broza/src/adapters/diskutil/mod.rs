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
//! is the private `assemble` module, which runs nothing at all.

mod assemble;
mod budget;
mod devices;
#[cfg(test)]
mod enumerator_tests;
mod hfs;
mod inputs;
pub(crate) mod parse;
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
#[cfg(test)]
mod volumes_tests;

use std::collections::BTreeSet;
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
use crate::adapters::tmutil_destinations::{self, BackupDestinations};
use crate::model::Warning;
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
/// Warning code for an enumeration that could not ask Time Machine anything.
pub const TM_DESTINATIONS_UNAVAILABLE_CODE: &str = "tm_destinations_unavailable";

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

    /// The volumes Time Machine backs up to, or none plus a warning.
    ///
    /// Time Machine's own answer is the only reliable way to recognise an APFS
    /// destination the user renamed. Losing it is not fatal — the heuristics
    /// in [`roles`] still catch the obvious cases — but the user has to be
    /// told that a backup disk might now look like an ordinary one.
    fn backup_destinations(&self, budget: &Budget, warnings: &mut Vec<Warning>) -> BackupDestinations {
        let timeout = match budget.next_timeout(DISKUTIL_TIMEOUT) {
            Ok(timeout) => timeout,
            Err(error) => {
                warnings.push(destinations_unavailable(&error));
                return BackupDestinations::none();
            }
        };
        match tmutil_destinations::backup_destinations(self.runner.as_ref(), timeout) {
            Ok(destinations) => destinations,
            Err(error) => {
                warnings.push(destinations_unavailable(&error));
                BackupDestinations::none()
            }
        }
    }
}

/// The warning for an enumeration that had to guess at backup volumes.
fn destinations_unavailable(error: &BrozaError) -> Warning {
    Warning {
        code: TM_DESTINATIONS_UNAVAILABLE_CODE.to_owned(),
        message: format!(
            "Time Machine destinations could not be read, so a backup volume may be reported as an \
             ordinary one: {error}"
        ),
        path: None,
    }
}

impl DiskEnumerator for DiskutilEnumerator {
    fn enumerate(&self) -> Result<EnumerationReport, BrozaError> {
        let budget = Budget::start(ENUMERATION_BUDGET);
        let list = parse_list(&self.capture(&["list", "-plist"], &budget)?)?;
        let apfs = parse_apfs_list(&self.capture(&["apfs", "list", "-plist"], &budget)?)?;
        let infos = self.collect_info(&list, &apfs, &budget)?;
        let mut warnings = Vec::new();
        let destinations = self.backup_destinations(&budget, &mut warnings);
        let inputs = Inputs {
            list: &list,
            apfs: &apfs,
            infos: &infos,
            destinations: &destinations,
            space: self.space.as_ref(),
            fs: self.fs.as_ref(),
        };
        let disks = assemble::assemble_disks(&inputs, &mut warnings);
        Ok(EnumerationReport { disks, warnings })
    }
}

/// Every device `diskutil info` has to be run for, in a stable order.
///
/// Physical disks and `HFS+` partitions for their model and their free space;
/// APFS volumes with no role and the data volumes, because both need
/// `WritableVolume` before Broza will call them writable. Duplicates are
/// dropped wherever they come from: one device must never cost two spawns.
fn devices_to_inspect(list: &DiskList, apfs: &ApfsList) -> Vec<String> {
    let physical = list.physical_devices().flat_map(|device| {
        std::iter::once(device.device_identifier.clone())
            .chain(device.hfs_partitions().map(|partition| partition.device_identifier.clone()))
    });
    let mounted_volumes = apfs
        .containers
        .iter()
        .flat_map(|container| &container.volumes)
        .filter(|volume| needs_writability_check(volume))
        .filter(|volume| {
            list.apfs_volume(&volume.device_identifier).is_some_and(|listed| listed.mount_point.is_some())
        })
        .map(|volume| volume.device_identifier.clone());
    let mut seen = BTreeSet::new();
    physical.chain(mounted_volumes).filter(|device| seen.insert(device.clone())).collect()
}

/// `true` for the volumes whose writability has to be observed, not assumed.
///
/// A volume with no role at all may turn out to be the user's disk, and a
/// `Data` volume is the one Broza actually cleans — neither may be called
/// writable on the strength of a role alone (`AGENTS.md` §2.3).
fn needs_writability_check(volume: &ApfsVolume) -> bool {
    volume.roles.is_empty() || roles_to_volume_role(&volume.roles).writable_by_broza()
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
