//! `diskutil list -plist`: the partition map of every device.
//!
//! This is the only place that knows which devices exist at all, which
//! partitions are `HFS+`, and where each APFS volume is mounted. Capacities and
//! roles come from `diskutil apfs list -plist` instead
//! ([`super::plist_apfs`]), because `list` does not report them.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::parse::{lenient_u64, optional_path, optional_text, parse_plist};
use crate::BrozaError;

/// Command this module parses, for error messages.
const COMMAND: &str = "diskutil list -plist";
/// `Content` of the synthesized device that represents an APFS container.
pub const APFS_CONTAINER_CONTENT: &str = "Apple_APFS_Container";
/// `Content` of an `HFS+` partition.
pub const HFS_PARTITION_CONTENT: &str = "Apple_HFS";
/// Value of `Sealed` on a mounted snapshot whose seal is intact.
const SEALED_YES: &str = "Yes";
/// The only mount point a sealed system snapshot may stand in for.
const ROOT_MOUNT_POINT: &str = "/";

/// The whole output of `diskutil list -plist`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct DiskList {
    /// Every whole device, physical or synthesized, with what is inside it.
    pub all_disks_and_partitions: Vec<ListDevice>,
}

impl DiskList {
    /// The devices that are real hardware rather than synthesized APFS containers.
    pub fn physical_devices(&self) -> impl Iterator<Item = &ListDevice> {
        self.all_disks_and_partitions.iter().filter(|device| !device.is_apfs_container())
    }

    /// The APFS volume entry of `device_identifier`, wherever it is listed.
    pub fn apfs_volume(&self, device_identifier: &str) -> Option<&ListApfsVolume> {
        self.all_disks_and_partitions
            .iter()
            .flat_map(|device| &device.apfs_volumes)
            .find(|volume| volume.device_identifier == device_identifier)
    }
}

/// One whole device: a physical disk, or the synthesized disk of an APFS container.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ListDevice {
    /// BSD identifier of the device (`disk0`).
    pub device_identifier: String,
    /// Partition scheme, or the marker of a synthesized container device.
    pub content: String,
    /// Capacity of the device in bytes.
    #[serde(deserialize_with = "lenient_u64")]
    pub size: u64,
    /// `true` for devices macOS hides from the user.
    #[serde(rename = "OSInternal")]
    pub os_internal: bool,
    /// Partitions of a physical device; empty for a container device.
    pub partitions: Vec<ListPartition>,
    /// Volumes of a container device; empty for a physical device.
    #[serde(rename = "APFSVolumes")]
    pub apfs_volumes: Vec<ListApfsVolume>,
}

impl ListDevice {
    /// `true` when this entry is the synthesized device of an APFS container.
    pub fn is_apfs_container(&self) -> bool {
        self.content == APFS_CONTAINER_CONTENT
    }

    /// The `HFS+` partitions of this device.
    pub fn hfs_partitions(&self) -> impl Iterator<Item = &ListPartition> {
        self.partitions.iter().filter(|partition| partition.is_hfs_plus())
    }
}

/// One partition of a physical device.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ListPartition {
    /// BSD identifier of the partition (`disk0s2`).
    pub device_identifier: String,
    /// Partition type (`Apple_APFS`, `Apple_HFS`, `EFI`, …).
    pub content: String,
    /// Capacity of the partition in bytes.
    #[serde(deserialize_with = "lenient_u64")]
    pub size: u64,
    /// Volume name, when the partition carries a mountable filesystem.
    #[serde(deserialize_with = "optional_text")]
    pub volume_name: Option<String>,
    /// Where the partition is mounted, when it is.
    #[serde(deserialize_with = "optional_path")]
    pub mount_point: Option<PathBuf>,
}

impl ListPartition {
    /// `true` for a Mac OS Extended partition.
    pub fn is_hfs_plus(&self) -> bool {
        self.content == HFS_PARTITION_CONTENT
    }
}

/// One APFS volume as `diskutil list` reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ListApfsVolume {
    /// BSD identifier of the volume (`disk3s5`).
    pub device_identifier: String,
    /// Volume name as Finder shows it.
    #[serde(deserialize_with = "optional_text")]
    pub volume_name: Option<String>,
    /// Where the volume itself is mounted.
    #[serde(deserialize_with = "optional_path")]
    pub mount_point: Option<PathBuf>,
    /// Capacity of the container the volume lives in, not of the volume.
    #[serde(deserialize_with = "lenient_u64")]
    pub size: u64,
    /// `true` for volumes macOS hides from the user.
    #[serde(rename = "OSInternal")]
    pub os_internal: bool,
    /// Snapshots of this volume that are themselves mounted.
    pub mounted_snapshots: Vec<ListMountedSnapshot>,
}

impl ListApfsVolume {
    /// Where this volume is reachable from, the sealed system snapshot included.
    ///
    /// On a sealed macOS the system volume is not mounted at `/`: the signed
    /// snapshot of it is, and the volume itself sits at
    /// `/System/Volumes/Update/mnt1`. The path a user can name is the
    /// snapshot's, so it wins — but only for that one case
    /// (`docs/cli-spec.md` §4.2 shows the system volume at `/`).
    ///
    /// The conditions are deliberately narrow: an intact seal *and* the root
    /// itself. Any other mounted snapshot — a Time Machine snapshot browsed
    /// under `/Volumes/com.apple.TimeMachine.…`, a snapshot a user mounted by
    /// hand — describes a point in the past, not the volume, and letting it
    /// supply the mount point would attribute live paths to it.
    pub fn effective_mount_point(&self) -> Option<PathBuf> {
        self.mounted_snapshots
            .iter()
            .find(|snapshot| snapshot.stands_in_for_the_volume())
            .and_then(|snapshot| snapshot.snapshot_mount_point.clone())
            .or_else(|| self.mount_point.clone())
    }
}

/// A mounted snapshot of an APFS volume.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ListMountedSnapshot {
    /// Snapshot name (`com.apple.os.update-…`).
    #[serde(deserialize_with = "optional_text")]
    pub snapshot_name: Option<String>,
    /// BSD identifier the snapshot is mounted as (`disk3s1s1`).
    #[serde(rename = "SnapshotBSD", deserialize_with = "optional_text")]
    pub snapshot_bsd: Option<String>,
    /// Seal state of the snapshot: `Yes`, `No`, or absent.
    #[serde(deserialize_with = "optional_text")]
    pub sealed: Option<String>,
    /// Where the snapshot is mounted.
    #[serde(deserialize_with = "optional_path")]
    pub snapshot_mount_point: Option<PathBuf>,
}

impl ListMountedSnapshot {
    /// `true` only for the sealed system snapshot mounted at `/`.
    pub fn stands_in_for_the_volume(&self) -> bool {
        self.sealed.as_deref() == Some(SEALED_YES)
            && self.snapshot_mount_point.as_deref() == Some(Path::new(ROOT_MOUNT_POINT))
    }
}

/// Parse the output of `diskutil list -plist`.
pub fn parse_list(bytes: &[u8]) -> Result<DiskList, BrozaError> {
    parse_plist(bytes, COMMAND)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{DiskList, parse_list};

    /// A minimal partition map: one physical disk with an `HFS+` partition, and
    /// the synthesized device of a container whose system volume is sealed.
    const SAMPLE: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>AllDisksAndPartitions</key>
  <array>
    <dict>
      <key>Content</key><string>GUID_partition_scheme</string>
      <key>DeviceIdentifier</key><string>disk0</string>
      <key>OSInternal</key><false/>
      <key>Size</key><integer>500277792768</integer>
      <key>Partitions</key>
      <array>
        <dict>
          <key>Content</key><string>Apple_APFS</string>
          <key>DeviceIdentifier</key><string>disk0s2</string>
          <key>Size</key><integer>494384795648</integer>
        </dict>
        <dict>
          <key>Content</key><string>Apple_HFS</string>
          <key>DeviceIdentifier</key><string>disk0s3</string>
          <key>MountPoint</key><string>/Volumes/Spare</string>
          <key>Size</key><integer>1000</integer>
          <key>VolumeName</key><string>Spare</string>
        </dict>
      </array>
    </dict>
    <dict>
      <key>Content</key><string>Apple_APFS_Container</string>
      <key>DeviceIdentifier</key><string>disk3</string>
      <key>Size</key><integer>494384795648</integer>
      <key>APFSVolumes</key>
      <array>
        <dict>
          <key>DeviceIdentifier</key><string>disk3s1</string>
          <key>MountPoint</key><string>/System/Volumes/Update/mnt1</string>
          <key>Size</key><integer>494384795648</integer>
          <key>VolumeName</key><string>Macintosh HD</string>
          <key>MountedSnapshots</key>
          <array>
            <dict>
              <key>Sealed</key><string>Yes</string>
              <key>SnapshotBSD</key><string>disk3s1s1</string>
              <key>SnapshotMountPoint</key><string>/</string>
              <key>SnapshotName</key><string>com.apple.os.update-abc</string>
            </dict>
          </array>
        </dict>
        <dict>
          <key>DeviceIdentifier</key><string>disk3s3</string>
          <key>MountPoint</key><string></string>
          <key>Size</key><integer>494384795648</integer>
          <key>VolumeName</key><string>Recovery</string>
        </dict>
        <dict>
          <key>DeviceIdentifier</key><string>disk3s5</string>
          <key>MountPoint</key><string>/System/Volumes/Data</string>
          <key>Size</key><integer>494384795648</integer>
          <key>VolumeName</key><string>Data</string>
          <key>MountedSnapshots</key>
          <array>
            <dict>
              <key>Sealed</key><string>No</string>
              <key>SnapshotBSD</key><string>disk3s5s1</string>
              <key>SnapshotMountPoint</key><string>/Volumes/com.apple.TimeMachine.localsnapshots</string>
              <key>SnapshotName</key><string>com.apple.TimeMachine.2026-09-20-101530.local</string>
            </dict>
          </array>
        </dict>
      </array>
    </dict>
  </array>
</dict>
</plist>"#;

    fn sample() -> DiskList {
        parse_list(SAMPLE).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn a_container_device_is_not_a_physical_one() {
        let list = sample();
        let ids: Vec<&str> =
            list.physical_devices().map(|device| device.device_identifier.as_str()).collect();

        assert_eq!(ids, vec!["disk0"]);
    }

    #[test]
    fn only_mac_os_extended_partitions_count_as_hfs_plus() {
        let list = sample();
        let device = list.physical_devices().next().unwrap_or_else(|| panic!("no physical device"));

        let hfs: Vec<&str> =
            device.hfs_partitions().map(|partition| partition.device_identifier.as_str()).collect();

        assert_eq!(hfs, vec!["disk0s3"], "the Apple_APFS partition is not a volume of its own");
    }

    #[test]
    fn a_sealed_system_volume_is_reported_at_the_mount_point_of_its_snapshot() {
        let list = sample();

        let volume = list.apfs_volume("disk3s1").unwrap_or_else(|| panic!("disk3s1 missing"));

        assert_eq!(volume.mount_point.as_deref(), Some(Path::new("/System/Volumes/Update/mnt1")));
        assert_eq!(volume.effective_mount_point().as_deref(), Some(Path::new("/")));
    }

    #[test]
    fn a_snapshot_that_is_not_the_sealed_root_never_supplies_the_mount_point() {
        let list = sample();

        let data = list.apfs_volume("disk3s5").unwrap_or_else(|| panic!("disk3s5 missing"));

        assert_eq!(
            data.effective_mount_point().as_deref(),
            Some(Path::new("/System/Volumes/Data")),
            "a browsed Time Machine snapshot is a view of the past, not the volume"
        );
    }

    #[test]
    fn a_snapshot_at_the_root_without_an_intact_seal_is_ignored_too() {
        let mut list = sample();
        let volume = list
            .all_disks_and_partitions
            .iter_mut()
            .flat_map(|device| &mut device.apfs_volumes)
            .find(|volume| volume.device_identifier == "disk3s1")
            .unwrap_or_else(|| panic!("disk3s1 missing"));
        volume.mounted_snapshots[0].sealed = Some("Broken".to_owned());

        let volume = list.apfs_volume("disk3s1").unwrap_or_else(|| panic!("disk3s1 missing"));

        assert_eq!(volume.effective_mount_point().as_deref(), Some(Path::new("/System/Volumes/Update/mnt1")));
    }

    #[test]
    fn an_unmounted_volume_has_no_mount_point_at_all() {
        let list = sample();

        let volume = list.apfs_volume("disk3s3").unwrap_or_else(|| panic!("disk3s3 missing"));

        assert_eq!(volume.effective_mount_point(), None);
        assert_eq!(volume.volume_name.as_deref(), Some("Recovery"));
    }

    #[test]
    fn a_volume_nobody_listed_is_simply_absent() {
        assert!(sample().apfs_volume("disk9s9").is_none());
    }

    #[test]
    fn an_empty_output_parses_into_an_empty_list() {
        let empty = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict/></plist>"#;

        let parsed = parse_list(empty).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(parsed, DiskList::default());
        assert_eq!(parsed.physical_devices().count(), 0);
    }
}
