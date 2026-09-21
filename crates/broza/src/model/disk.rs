//! Disks, containers, volumes and snapshots (`docs/cli-spec.md` §4.2 and §4.3).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::ids::VolumeId;

/// A physical disk as reported by `diskutil`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Disk {
    /// BSD identifier of the whole device (`disk0`).
    pub id: VolumeId,
    /// Marketing model name of the device.
    pub model: String,
    /// Total capacity of the device in bytes.
    pub size_bytes: u64,
    /// `true` for internal devices, `false` for external ones.
    pub internal: bool,
    /// Containers carved out of the device.
    #[serde(default)]
    pub containers: Vec<Container>,
}

/// A filesystem container (an APFS container, or a single `HFS+` partition).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Container {
    /// BSD identifier of the container (`disk3`).
    pub id: VolumeId,
    /// Filesystem family of the container.
    #[serde(rename = "type")]
    pub kind: FsKind,
    /// Capacity of the container in bytes.
    pub size_bytes: u64,
    /// Bytes in use across every volume of the container.
    pub used_bytes: u64,
    /// Bytes reported as free by the filesystem.
    pub free_bytes: u64,
    /// Estimated purgeable bytes. Never summed into `free_bytes` (`AGENTS.md` §2.7).
    pub purgeable_bytes: u64,
    /// Volumes inside the container.
    #[serde(default)]
    pub volumes: Vec<Volume>,
}

/// Filesystem family of a [`Container`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FsKind {
    /// Apple File System.
    Apfs,
    /// Mac OS Extended (`HFS+`).
    HfsPlus,
    /// Anything Broza does not model; unknown values deserialise here.
    #[serde(other)]
    Unknown,
}

/// A mounted or mountable volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Volume {
    /// BSD identifier of the volume (`disk3s1`).
    pub id: VolumeId,
    /// Volume name as shown in Finder.
    pub name: String,
    /// Role assigned by macOS.
    pub role: VolumeRole,
    /// Mount point. Absent when the volume is not mounted (`Preboot`, `Recovery`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_point: Option<PathBuf>,
    /// Bytes in use on this volume.
    pub used_bytes: u64,
    /// `true` when Broza is allowed to write to the volume; mirrors
    /// [`VolumeRole::writable_by_broza`].
    pub writable_by_broza: bool,
    /// One-sentence explanation of what the volume is for.
    pub purpose: String,
}

/// Role of a volume (`docs/cli-spec.md` §4.1, stable enum `role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VolumeRole {
    /// Sealed, read-only system volume.
    System,
    /// The writable data volume of a system group.
    Data,
    /// Boot loader volume.
    Preboot,
    /// Recovery environment volume.
    Recovery,
    /// Virtual memory (swap) volume.
    Vm,
    /// Time Machine backup volume.
    Backup,
    /// A user data volume, typically an external disk.
    User,
    /// Role Broza could not determine; unknown values deserialise here.
    #[serde(other)]
    Unknown,
}

impl VolumeRole {
    /// `true` only for [`VolumeRole::Data`] and [`VolumeRole::User`].
    ///
    /// Volumes with role `system`, `preboot`, `recovery` or `vm` are read-only for
    /// Broza and no flag can change that (`AGENTS.md` §2.3). `backup` volumes are
    /// only ever touched through `tmutil`, never written to directly.
    pub const fn writable_by_broza(self) -> bool {
        matches!(self, Self::Data | Self::User)
    }
}

/// An APFS snapshot (`docs/cli-spec.md` §4.3, `snapshots` findings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Snapshot {
    /// Snapshot name, for example `com.apple.TimeMachine.2026-09-20-101530.local`.
    pub name: String,
    /// Snapshot UUID when macOS reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    /// `true` when macOS marks the snapshot as purgeable. Only purgeable Time Machine
    /// snapshots are actionable; `com.apple.os.update-*` snapshots never are.
    pub purgeable: bool,
}

#[cfg(test)]
mod tests {
    use super::{FsKind, Snapshot, Volume, VolumeRole};

    #[test]
    fn only_data_and_user_volumes_are_writable() {
        let writable = [VolumeRole::Data, VolumeRole::User];
        let protected = [
            VolumeRole::System,
            VolumeRole::Preboot,
            VolumeRole::Recovery,
            VolumeRole::Vm,
            VolumeRole::Backup,
            VolumeRole::Unknown,
        ];
        for role in writable {
            assert!(role.writable_by_broza(), "{role:?}");
        }
        for role in protected {
            assert!(!role.writable_by_broza(), "{role:?}");
        }
    }

    #[test]
    fn roles_use_the_stable_identifiers_of_the_specification() {
        let cases = [
            (VolumeRole::System, "\"system\""),
            (VolumeRole::Data, "\"data\""),
            (VolumeRole::Preboot, "\"preboot\""),
            (VolumeRole::Recovery, "\"recovery\""),
            (VolumeRole::Vm, "\"vm\""),
            (VolumeRole::Backup, "\"backup\""),
            (VolumeRole::User, "\"user\""),
            (VolumeRole::Unknown, "\"unknown\""),
        ];
        for (role, expected) in cases {
            let json = serde_json::to_string(&role).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(json, expected);
            let back: VolumeRole = serde_json::from_str(expected).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(back, role);
        }
    }

    #[test]
    fn unknown_filesystem_kinds_do_not_fail_to_deserialize() {
        let kind: FsKind = serde_json::from_str("\"zfs\"").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(kind, FsKind::Unknown);
        let known: FsKind = serde_json::from_str("\"hfs_plus\"").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(known, FsKind::HfsPlus);
    }

    #[test]
    fn an_unmounted_volume_omits_its_mount_point() {
        let volume = Volume {
            id: "disk3s2".parse().unwrap_or_else(|e| panic!("{e}")),
            name: "Preboot".into(),
            role: VolumeRole::Preboot,
            mount_point: None,
            used_bytes: 0,
            writable_by_broza: false,
            purpose: "Boot loader volume.".into(),
        };
        let json = serde_json::to_value(&volume).unwrap_or_else(|e| panic!("{e}"));
        assert!(json.get("mount_point").is_none());
    }

    #[test]
    fn a_snapshot_without_a_uuid_omits_the_field() {
        let snapshot = Snapshot { name: "com.apple.TimeMachine.x".into(), uuid: None, purgeable: true };
        let json = serde_json::to_value(&snapshot).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(json, serde_json::json!({"name": "com.apple.TimeMachine.x", "purgeable": true}));
    }
}
