//! Which volumes Time Machine backs up to, according to Time Machine.
//!
//! Recognising a backup destination by what is on it — a `Backups.backupdb`
//! directory, a name with "Time Machine" in it — works for `HFS+` disks and
//! for people who left the default name alone. An APFS destination keeps its
//! backups in snapshots, not in a directory anyone can see, and a user who
//! called the disk `Backup4TB` defeats the name check. Such a volume is
//! writable and mounted under `/Volumes`, so without this module it would be
//! classified `user` and Broza would consider itself free to move files out of
//! somebody's backups.
//!
//! `tmutil destinationinfo -X` is the authority on the question, so it is
//! asked once per enumeration and its answer outranks every heuristic
//! (`AGENTS.md` §2.3).
//!
//! The matching is deliberately generous — mount point, destination id, or
//! name. The two mistakes are not equal: calling a volume a backup when it is
//! not costs it its writability, while missing one would let Broza write into
//! backups.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::BrozaError;
use crate::adapters::diskutil::parse::{optional_path, optional_text, parse_plist};
use crate::ports::ProcessRunner;

/// Absolute path of `tmutil`; never resolved through `PATH`.
pub const TMUTIL: &str = "/usr/bin/tmutil";
/// Arguments that make `tmutil` list destinations as a property list.
pub const DESTINATION_INFO_ARGS: [&str; 2] = ["destinationinfo", "-X"];
/// Command this module parses, for error messages.
const COMMAND: &str = "tmutil destinationinfo -X";

/// The volumes Time Machine backs up to.
///
/// Empty when Time Machine is not configured, and also when `tmutil` could not
/// be asked — the caller turns that into a warning and falls back to the
/// heuristics in [`crate::adapters::diskutil::roles`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupDestinations {
    /// Where the destinations are mounted.
    mount_points: BTreeSet<PathBuf>,
    /// Destination names, as Time Machine shows them.
    names: BTreeSet<String>,
    /// Destination identifiers, which are volume UUIDs for local disks.
    ids: BTreeSet<String>,
}

impl BackupDestinations {
    /// No destination at all: every question answers "no".
    pub fn none() -> Self {
        Self::default()
    }

    /// `true` when Time Machine named no destination.
    pub fn is_empty(&self) -> bool {
        self.mount_points.is_empty() && self.names.is_empty() && self.ids.is_empty()
    }

    /// `true` when a volume is one of the destinations.
    ///
    /// Names are compared case-insensitively, the way a user reads them; an
    /// empty name matches nothing, because half the volumes on a Mac have one.
    pub fn contains(&self, mount_point: Option<&Path>, name: &str, uuid: Option<&str>) -> bool {
        if mount_point.is_some_and(|path| self.mount_points.contains(path)) {
            return true;
        }
        if uuid.is_some_and(|uuid| self.ids.iter().any(|known| known.eq_ignore_ascii_case(uuid))) {
            return true;
        }
        !name.is_empty() && self.names.iter().any(|known| known.eq_ignore_ascii_case(name))
    }
}

/// The destinations Time Machine knows about, asked of `tmutil`.
///
/// A `tmutil` that cannot be run, or that refuses, is an error for the caller
/// to report as a warning: Broza then knows less than it would like, and says
/// so, rather than pretending there are no backups.
pub fn backup_destinations(
    runner: &dyn ProcessRunner,
    timeout: Duration,
) -> Result<BackupDestinations, BrozaError> {
    let output = runner.run(TMUTIL, &DESTINATION_INFO_ARGS, timeout)?;
    if !output.success {
        return Err(BrozaError::Other(format!(
            "`{COMMAND}` failed: {}",
            crate::adapters::diskutil::first_line(&output.stderr_text())
        )));
    }
    parse_destination_info(&output.stdout)
}

/// Parse the output of `tmutil destinationinfo -X`.
pub fn parse_destination_info(bytes: &[u8]) -> Result<BackupDestinations, BrozaError> {
    let parsed: DestinationInfo = parse_plist(bytes, COMMAND)?;
    let mut destinations = BackupDestinations::none();
    for destination in parsed.destinations {
        destinations.mount_points.extend(destination.mount_point);
        destinations.names.extend(destination.name);
        destinations.ids.extend(destination.id);
    }
    Ok(destinations)
}

/// The whole output of `tmutil destinationinfo -X`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct DestinationInfo {
    /// Every destination, local disk or network share.
    destinations: Vec<Destination>,
}

/// One Time Machine destination.
///
/// `Kind` is read but not filtered on: a network destination normally has a
/// `URL` and no mount point, so it contributes nothing, and on the day one
/// does report a mount point, treating it as a backup is the careful answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct Destination {
    /// `Local` for a disk, `Network` for a share.
    #[serde(deserialize_with = "optional_text")]
    kind: Option<String>,
    /// Name Time Machine shows for the destination.
    #[serde(deserialize_with = "optional_text")]
    name: Option<String>,
    /// Destination identifier; the volume UUID for a local disk.
    #[serde(rename = "ID", deserialize_with = "optional_text")]
    id: Option<String>,
    /// Where the destination is mounted, for a local disk.
    #[serde(deserialize_with = "optional_path")]
    mount_point: Option<PathBuf>,
    /// Share the backups live on, for a network destination.
    #[serde(rename = "URL", deserialize_with = "optional_text")]
    url: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use super::{
        BackupDestinations, DESTINATION_INFO_ARGS, TMUTIL, backup_destinations, parse_destination_info,
    };
    use crate::BrozaError;
    use crate::ports::{ProcessOutput, ProcessRunner};
    use crate::testing::FakeRunner;

    /// One local destination on an external disk and one network share.
    const TWO_DESTINATIONS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>Destinations</key>
  <array>
    <dict>
      <key>ID</key><string>00000101-1111-4222-8333-000000000101</string>
      <key>Kind</key><string>Local</string>
      <key>MountPoint</key><string>/Volumes/Backup4TB</string>
      <key>Name</key><string>Backup4TB</string>
    </dict>
    <dict>
      <key>ID</key><string>00000102-1111-4222-8333-000000000102</string>
      <key>Kind</key><string>Network</string>
      <key>Name</key><string>Time Capsule</string>
      <key>URL</key><string>smb://testuser@test-mac/Backups</string>
    </dict>
  </array>
</dict>
</plist>"#;
    /// What a Mac with Time Machine switched off answers.
    const NO_DESTINATIONS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict></dict></plist>"#;
    /// Timeout the tests give `tmutil`.
    const TIMEOUT: Duration = Duration::from_secs(5);

    fn two() -> BackupDestinations {
        parse_destination_info(TWO_DESTINATIONS).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn a_destination_is_recognised_by_its_mount_point() {
        assert!(two().contains(Some(Path::new("/Volumes/Backup4TB")), "", None));
    }

    #[test]
    fn a_destination_is_recognised_by_its_identifier_whatever_it_is_mounted_as() {
        let uuid = "00000101-1111-4222-8333-000000000101";

        assert!(two().contains(Some(Path::new("/Volumes/Moved")), "Moved", Some(uuid)));
        assert!(two().contains(None, "", Some(&uuid.to_lowercase())));
    }

    #[test]
    fn a_destination_is_recognised_by_its_name_however_it_is_capitalised() {
        assert!(two().contains(Some(Path::new("/Volumes/Elsewhere")), "backup4tb", None));
        assert!(two().contains(None, "Time Capsule", None), "a network share is a destination too");
    }

    #[test]
    fn a_volume_nobody_backs_up_to_is_not_a_destination() {
        let destinations = two();

        assert!(!destinations.contains(Some(Path::new("/Volumes/Scratch")), "Scratch", None));
        assert!(!destinations.contains(None, "", None));
        assert!(
            !destinations.contains(Some(Path::new("/System/Volumes/Data")), "", None),
            "an empty name must not match the destinations that have one"
        );
    }

    #[test]
    fn a_mac_without_time_machine_has_no_destinations() {
        let destinations = parse_destination_info(NO_DESTINATIONS).unwrap_or_else(|e| panic!("{e}"));

        assert!(destinations.is_empty());
        assert!(!destinations.contains(Some(Path::new("/Volumes/Backup4TB")), "Backup4TB", None));
        assert_eq!(destinations, BackupDestinations::none());
    }

    #[test]
    fn the_command_is_the_absolute_path_with_the_timeout_it_was_given() {
        let runner = Arc::new(FakeRunner::new().with_output(
            TMUTIL,
            &DESTINATION_INFO_ARGS,
            ProcessOutput {
                success: true,
                code: Some(0),
                stdout: TWO_DESTINATIONS.to_vec(),
                stderr: Vec::new(),
            },
        ));

        let destinations = backup_destinations(runner.as_ref() as &dyn ProcessRunner, TIMEOUT)
            .unwrap_or_else(|e| panic!("{e}"));

        assert!(!destinations.is_empty());
        assert_eq!(runner.calls()[0].program, TMUTIL);
        assert_eq!(runner.calls()[0].timeout, TIMEOUT);
    }

    #[test]
    fn a_tmutil_that_refuses_is_an_error_the_caller_can_warn_about() {
        let runner = FakeRunner::new().with_output(
            TMUTIL,
            &DESTINATION_INFO_ARGS,
            ProcessOutput {
                success: false,
                code: Some(1),
                stdout: Vec::new(),
                stderr: b"tmutil: destinationinfo requires Full Disk Access\n".to_vec(),
            },
        );

        let err = backup_destinations(&runner, TIMEOUT).err();

        let Some(BrozaError::Other(message)) = err else { panic!("expected BrozaError::Other") };
        assert!(message.contains("Full Disk Access"), "{message}");
    }

    #[test]
    fn output_that_is_not_a_plist_is_an_error_and_not_a_panic() {
        assert!(parse_destination_info(b"tmutil: no destinations configured").is_err());
    }
}
