//! `diskutil info -plist <device>`: what one device or volume is.
//!
//! Two callers, two reasons. For a physical disk this is the only source of the
//! model name and of whether the disk is internal (`docs/cli-spec.md` §4.2). For
//! an `HFS+` partition it is the only source of free space, because
//! `diskutil list` reports a partition's size and nothing else.

use std::path::PathBuf;

use serde::Deserialize;

use super::parse::{optional_path, optional_text, parse_plist};
use crate::BrozaError;

/// Command this module parses, for error messages.
const COMMAND: &str = "diskutil info -plist";
/// Value of `Sealed` on a volume whose seal is intact.
const SEALED_YES: &str = "Yes";

/// The subset of `diskutil info -plist` Broza reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct DeviceInfo {
    /// Marketing name of the medium (`APPLE SSD AP0512Z`, `Disk Image`).
    #[serde(deserialize_with = "optional_text")]
    pub media_name: Option<String>,
    /// BSD identifier of the device this output describes.
    pub device_identifier: String,
    /// `true` for devices attached to the internal bus.
    pub internal: bool,
    /// `true` for flash storage. Absent on disk images, and then `false`.
    pub solid_state: bool,
    /// Capacity of the device in bytes.
    pub total_size: u64,
    /// Bytes free on the mounted filesystem; `0` when nothing is mounted.
    pub free_space: u64,
    /// Where the device is mounted, when it is.
    #[serde(deserialize_with = "optional_path")]
    pub mount_point: Option<PathBuf>,
    /// Seal state of a signed system volume: `Yes`, `No` or `Broken`.
    #[serde(deserialize_with = "optional_text")]
    pub sealed: Option<String>,
    /// Filesystem of the volume (`apfs`, `hfs`), when it carries one.
    #[serde(deserialize_with = "optional_text")]
    pub filesystem_type: Option<String>,
    /// Container this volume belongs to, for APFS volumes.
    #[serde(rename = "APFSContainerReference", deserialize_with = "optional_text")]
    pub apfs_container_reference: Option<String>,
}

impl DeviceInfo {
    /// `true` only when macOS reports the seal as intact.
    ///
    /// A broken seal is not a sealed volume, and a volume that reports nothing
    /// is not one either: only the explicit `Yes` counts.
    pub fn is_sealed(&self) -> bool {
        self.sealed.as_deref() == Some(SEALED_YES)
    }

    /// Bytes in use on the mounted filesystem.
    ///
    /// Saturating: a device whose reported free space exceeds its size must not
    /// wrap around into an enormous "used".
    pub fn used_bytes(&self) -> u64 {
        self.total_size.saturating_sub(self.free_space)
    }
}

/// Parse the output of `diskutil info -plist <device>`.
pub fn parse_info(bytes: &[u8]) -> Result<DeviceInfo, BrozaError> {
    parse_plist(bytes, COMMAND)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{DeviceInfo, parse_info};

    /// A mounted `HFS+` volume on a disk image: no `SolidState`, a real
    /// `FreeSpace`, no seal.
    const DISK_IMAGE: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>DeviceIdentifier</key><string>disk4s1</string>
  <key>FilesystemType</key><string>hfs</string>
  <key>FreeSpace</key><integer>2772992</integer>
  <key>Internal</key><false/>
  <key>MediaName</key><string>Disk Image</string>
  <key>MountPoint</key><string>/Volumes/Installer</string>
  <key>TotalSize</key><integer>1878605824</integer>
</dict>
</plist>"#;

    fn disk_image() -> DeviceInfo {
        parse_info(DISK_IMAGE).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn a_mounted_volume_reports_what_it_uses() {
        let info = disk_image();

        assert_eq!(info.used_bytes(), 1_875_832_832);
        assert_eq!(info.mount_point.as_deref(), Some(Path::new("/Volumes/Installer")));
        assert_eq!(info.filesystem_type.as_deref(), Some("hfs"));
    }

    #[test]
    fn a_missing_solid_state_key_is_not_a_solid_state_disk() {
        assert!(!disk_image().solid_state);
        assert!(!disk_image().internal);
    }

    #[test]
    fn only_an_intact_seal_counts_as_sealed() {
        let mut info = disk_image();
        assert!(!info.is_sealed(), "a volume with no seal is not sealed");

        info.sealed = Some("Broken".to_owned());
        assert!(!info.is_sealed(), "a broken seal is not a seal");

        info.sealed = Some("Yes".to_owned());
        assert!(info.is_sealed());
    }

    #[test]
    fn free_space_larger_than_the_device_reports_nothing_in_use() {
        let mut info = disk_image();
        info.free_space = info.total_size + 1;

        assert_eq!(info.used_bytes(), 0);
    }

    #[test]
    fn an_empty_output_parses_into_an_empty_device() {
        let empty = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict/></plist>"#;

        assert_eq!(parse_info(empty).unwrap_or_else(|e| panic!("{e}")), DeviceInfo::default());
    }
}
