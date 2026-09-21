//! `diskutil apfs list -plist`: containers, their capacities and their volumes.
//!
//! This is the authoritative source for everything `diskutil list` does not
//! report: container capacity and free space, volume roles, bytes in use and
//! encryption state (`docs/adr/0002-diskutil-plist-over-diskarbitration.md`).

use serde::Deserialize;

use super::parse::{lenient_u64, optional_text, parse_plist};
use crate::BrozaError;

/// Command this module parses, for error messages.
const COMMAND: &str = "diskutil apfs list -plist";

/// The whole output of `diskutil apfs list -plist`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ApfsList {
    /// Every APFS container known to the system.
    pub containers: Vec<ApfsContainer>,
}

/// One APFS container.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ApfsContainer {
    /// UUID of the container.
    #[serde(rename = "APFSContainerUUID", deserialize_with = "optional_text")]
    pub apfs_container_uuid: Option<String>,
    /// BSD identifier of the synthesized container device (`disk3`).
    pub container_reference: String,
    /// Capacity of the container in bytes.
    #[serde(deserialize_with = "lenient_u64")]
    pub capacity_ceiling: u64,
    /// Bytes still free in the container.
    #[serde(deserialize_with = "lenient_u64")]
    pub capacity_free: u64,
    /// Partitions the container is built from.
    pub physical_stores: Vec<ApfsPhysicalStore>,
    /// Volumes sharing the space of the container.
    pub volumes: Vec<ApfsVolume>,
}

impl ApfsContainer {
    /// Bytes in use across every volume of the container.
    ///
    /// `diskutil` reports a ceiling and what is free; used is the difference,
    /// saturating so that a container reporting more free than it has cannot
    /// wrap around into a gigantic "used".
    pub fn used_bytes(&self) -> u64 {
        self.capacity_ceiling.saturating_sub(self.capacity_free)
    }
}

/// A partition backing an APFS container.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ApfsPhysicalStore {
    /// BSD identifier of the partition (`disk0s2`).
    pub device_identifier: String,
    /// Capacity of the partition in bytes.
    #[serde(deserialize_with = "lenient_u64")]
    pub size: u64,
}

/// One APFS volume inside a container.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ApfsVolume {
    /// UUID of the volume.
    #[serde(rename = "APFSVolumeUUID", deserialize_with = "optional_text")]
    pub apfs_volume_uuid: Option<String>,
    /// BSD identifier of the volume (`disk3s5`).
    pub device_identifier: String,
    /// Volume name as Finder shows it.
    #[serde(deserialize_with = "optional_text")]
    pub name: Option<String>,
    /// Roles macOS assigned to the volume, verbatim (`System`, `Data`, `VM`, …).
    pub roles: Vec<String>,
    /// Bytes this volume occupies in the shared container space.
    #[serde(deserialize_with = "lenient_u64")]
    pub capacity_in_use: u64,
    /// `true` when the volume is encrypted.
    pub encryption: bool,
    /// `true` when the volume is a `FileVault` volume.
    #[serde(rename = "FileVault")]
    pub file_vault: bool,
    /// `true` when the volume is locked and its contents unreadable.
    pub locked: bool,
}

/// Parse the output of `diskutil apfs list -plist`.
pub fn parse_apfs_list(bytes: &[u8]) -> Result<ApfsList, BrozaError> {
    parse_plist(bytes, COMMAND)
}

#[cfg(test)]
mod tests {
    use super::{ApfsList, parse_apfs_list};

    /// One container with a system volume and a locked data volume.
    const SAMPLE: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>Containers</key>
  <array>
    <dict>
      <key>APFSContainerUUID</key><string>00000001-1111-4222-8333-000000000001</string>
      <key>CapacityCeiling</key><integer>1000</integer>
      <key>CapacityFree</key><integer>400</integer>
      <key>ContainerReference</key><string>disk3</string>
      <key>PhysicalStores</key>
      <array>
        <dict>
          <key>DeviceIdentifier</key><string>disk0s2</string>
          <key>Size</key><integer>1000</integer>
        </dict>
      </array>
      <key>Volumes</key>
      <array>
        <dict>
          <key>APFSVolumeUUID</key><string>00000002-1111-4222-8333-000000000002</string>
          <key>CapacityInUse</key><integer>500</integer>
          <key>DeviceIdentifier</key><string>disk3s1</string>
          <key>Encryption</key><true/>
          <key>FileVault</key><true/>
          <key>Locked</key><false/>
          <key>Name</key><string>Macintosh HD</string>
          <key>Roles</key><array><string>System</string></array>
        </dict>
        <dict>
          <key>CapacityInUse</key><integer>100</integer>
          <key>DeviceIdentifier</key><string>disk3s5</string>
          <key>Locked</key><true/>
          <key>Name</key><string>Data</string>
          <key>Roles</key><array><string>Data</string></array>
        </dict>
      </array>
    </dict>
  </array>
</dict>
</plist>"#;

    fn sample() -> ApfsList {
        parse_apfs_list(SAMPLE).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn used_space_is_the_ceiling_minus_what_is_free() {
        let list = sample();
        let container = list.containers.first().unwrap_or_else(|| panic!("no container"));

        assert_eq!(container.used_bytes(), 600);
        assert_eq!(container.container_reference, "disk3");
        assert_eq!(container.physical_stores[0].device_identifier, "disk0s2");
    }

    #[test]
    fn a_container_reporting_more_free_than_it_holds_reports_no_used_space() {
        let mut list = sample();
        let container = list.containers.first_mut().unwrap_or_else(|| panic!("no container"));
        container.capacity_free = container.capacity_ceiling + 1;

        assert_eq!(container.used_bytes(), 0);
    }

    #[test]
    fn roles_and_encryption_flags_survive_the_parse() {
        let list = sample();
        let volumes = &list.containers[0].volumes;

        assert_eq!(volumes[0].roles, vec!["System".to_owned()]);
        assert!(volumes[0].encryption && volumes[0].file_vault && !volumes[0].locked);
        assert_eq!(volumes[1].name.as_deref(), Some("Data"));
        assert!(volumes[1].locked, "a locked volume must be reported as locked");
        assert!(!volumes[1].encryption, "a missing key defaults to false");
        assert_eq!(volumes[1].apfs_volume_uuid, None);
    }

    #[test]
    fn a_machine_without_apfs_containers_parses_into_an_empty_list() {
        let empty = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict/></plist>"#;

        assert_eq!(parse_apfs_list(empty).unwrap_or_else(|e| panic!("{e}")), ApfsList::default());
    }
}
