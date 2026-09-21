//! Building one container and its volumes, and deciding what may be written to.
//!
//! Every volume that reaches the JSON contract passes through here, so this is
//! where "is this the user's disk?" is answered. It is answered by
//! [`super::roles::volume_role`] from facts Broza actually observed — the roles
//! `diskutil apfs list` reports, `WritableVolume` from `diskutil info`, the
//! volume's own mount point and a Time Machine marker on disk — and never by
//! the filesystem family alone (`AGENTS.md` §2.3).

use std::path::Path;

use super::devices::ordered_by_device;
use super::inputs::{Inputs, warning};
use super::plist_apfs::{ApfsContainer, ApfsVolume};
use super::plist_info::DeviceInfo;
use super::plist_list::ListApfsVolume;
use super::purpose::purpose_for_volume;
use super::roles::{TIME_MACHINE_MARKER, VolumeFacts, volume_role};
use crate::model::{Container, FsKind, Volume, VolumeId, VolumeRole, Warning};

/// Purgeable bytes reported when macOS will not answer.
pub(super) const UNKNOWN_PURGEABLE_BYTES: u64 = 0;
/// Warning code for a device `diskutil` named in a way Broza cannot parse.
pub(crate) const UNREADABLE_ID_CODE: &str = "unreadable_device_id";
/// Warning code for a data volume macOS mounted read-only.
pub(crate) const DATA_NOT_WRITABLE_CODE: &str = "data_volume_not_writable";

/// One APFS container with its volumes, or `None` when it cannot be identified.
pub(crate) fn apfs_container(
    container: &ApfsContainer,
    inputs: &Inputs<'_>,
    warnings: &mut Vec<Warning>,
) -> Option<Container> {
    let Ok(id) = container.container_reference.parse::<VolumeId>() else {
        warnings.push(warning(
            UNREADABLE_ID_CODE,
            format!(
                "skipped an APFS container: `{}` is not a BSD device name",
                container.container_reference
            ),
            None,
        ));
        return None;
    };
    let volumes = ordered_by_device(
        container.volumes.iter().filter_map(|volume| apfs_volume(volume, inputs, warnings)).collect(),
        |volume| volume.id.as_str(),
    );
    Some(Container {
        id,
        kind: FsKind::Apfs,
        size_bytes: container.capacity_ceiling,
        used_bytes: container.used_bytes(),
        free_bytes: container.capacity_free,
        purgeable_bytes: purgeable_of(&volumes, inputs),
        volumes,
    })
}

/// One APFS volume of a container.
fn apfs_volume(volume: &ApfsVolume, inputs: &Inputs<'_>, warnings: &mut Vec<Warning>) -> Option<Volume> {
    let Ok(id) = volume.device_identifier.parse::<VolumeId>() else {
        warnings.push(warning(
            UNREADABLE_ID_CODE,
            format!("skipped a volume: `{}` is not a BSD device name", volume.device_identifier),
            None,
        ));
        return None;
    };
    let listed = inputs.list.apfs_volume(&volume.device_identifier);
    let name = volume
        .name
        .clone()
        .or_else(|| listed.and_then(|listed| listed.volume_name.clone()))
        .unwrap_or_default();
    // The volume's own mount point decides the role; the snapshot standing in
    // for a sealed system volume decides only what Broza reports.
    let own_mount_point = listed.and_then(|listed| listed.mount_point.clone());
    let info = inputs.infos.get(&volume.device_identifier);
    let observed = facts(info, &name, volume.apfs_volume_uuid.as_deref(), own_mount_point.as_deref(), inputs);
    let role = volume_role(&volume.roles, own_mount_point.as_deref(), observed);
    Some(Volume {
        id,
        purpose: purpose_for_volume(role, &volume.roles, &name),
        writable_by_broza: writable(role, observed, &name, warnings),
        name,
        role,
        mount_point: listed.and_then(ListApfsVolume::effective_mount_point),
        used_bytes: volume.capacity_in_use,
    })
}

/// Whether Broza may write to a volume: its role must allow it *and* macOS
/// must agree that the volume is writable.
///
/// The role is the first gate and no flag can open it (`AGENTS.md` §2.3). The
/// second gate is `WritableVolume`: a data volume mounted read-only — by
/// `FileVault` before unlock, by a failing disk macOS remounted read-only, by a
/// recovery boot — is not somewhere Broza can move files to, and planning a
/// cleanup for it would only produce failures at apply time. The disagreement
/// is worth saying out loud, so it becomes a warning.
pub(super) fn writable(
    role: VolumeRole,
    facts: VolumeFacts<'_>,
    name: &str,
    warnings: &mut Vec<Warning>,
) -> bool {
    if !role.writable_by_broza() {
        return false;
    }
    if facts.writable_volume {
        return true;
    }
    if role == VolumeRole::Data {
        warnings.push(warning(
            DATA_NOT_WRITABLE_CODE,
            format!(
                "the data volume {} is mounted read-only, so Broza cannot clean anything on it",
                display_name(name)
            ),
            None,
        ));
    }
    false
}

/// How a warning refers to a volume that may have no name.
fn display_name(name: &str) -> &str {
    if name.is_empty() { "(unnamed)" } else { name }
}

/// Everything Broza observed about a volume besides its declared roles.
pub(super) fn facts<'a>(
    info: Option<&'a DeviceInfo>,
    name: &'a str,
    uuid: Option<&str>,
    mount_point: Option<&Path>,
    inputs: &Inputs<'_>,
) -> VolumeFacts<'a> {
    VolumeFacts {
        writable_volume: info.is_some_and(|info| info.writable_volume),
        backup_destination: inputs.destinations.contains(mount_point, name, uuid),
        time_machine_marker: mount_point
            .is_some_and(|mount_point| inputs.fs.exists(&mount_point.join(TIME_MACHINE_MARKER))),
        content: info.and_then(|info| info.content.as_deref()),
        name: Some(name),
    }
}

/// Purgeable bytes of a container: the largest estimate its own volumes report.
///
/// The question is asked of every mounted volume Broza considers the user's —
/// role `data` or `user` — because a container can hold more than one. They all
/// share the same free space, so the answers describe the same pool and the
/// largest is the estimate for the container; summing them would count the same
/// bytes twice. A container with no such volume, or a mount point macOS
/// refuses to answer for, reports zero: an estimate nobody made is not a guess
/// Broza invents (`AGENTS.md` §2.7).
pub(super) fn purgeable_of(volumes: &[Volume], inputs: &Inputs<'_>) -> u64 {
    volumes
        .iter()
        .filter(|volume| matches!(volume.role, VolumeRole::Data | VolumeRole::User))
        .filter_map(|volume| volume.mount_point.as_ref())
        .filter_map(|mount_point| inputs.space.purgeable_bytes(mount_point).ok())
        .max()
        .unwrap_or(UNKNOWN_PURGEABLE_BYTES)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::apfs_container;
    use crate::adapters::diskutil::plist_apfs::{ApfsContainer, ApfsVolume};
    use crate::adapters::diskutil::plist_info::DeviceInfo;
    use crate::adapters::diskutil::plist_list::{DiskList, ListApfsVolume, ListDevice};
    use crate::adapters::diskutil::tests_support::Scenario;
    use crate::model::{VolumeRole, Warning};

    fn info(id: &str, mount: Option<&str>, writable: bool) -> DeviceInfo {
        DeviceInfo {
            device_identifier: id.to_owned(),
            total_size: 1_000,
            free_space: 250,
            writable_volume: writable,
            mount_point: mount.map(PathBuf::from),
            ..DeviceInfo::default()
        }
    }

    fn apfs_volume(id: &str, name: &str, roles: &[&str]) -> ApfsVolume {
        ApfsVolume {
            device_identifier: id.to_owned(),
            name: Some(name.to_owned()),
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            capacity_in_use: 100,
            ..ApfsVolume::default()
        }
    }

    fn container(volumes: Vec<ApfsVolume>) -> ApfsContainer {
        ApfsContainer {
            container_reference: "disk3".to_owned(),
            capacity_ceiling: 1_000,
            capacity_free: 400,
            volumes,
            ..ApfsContainer::default()
        }
    }

    fn listed(id: &str, mount: Option<&str>) -> ListDevice {
        ListDevice {
            device_identifier: "disk3".to_owned(),
            content: "Apple_APFS_Container".to_owned(),
            apfs_volumes: vec![ListApfsVolume {
                device_identifier: id.to_owned(),
                mount_point: mount.map(PathBuf::from),
                ..ListApfsVolume::default()
            }],
            ..ListDevice::default()
        }
    }

    #[test]
    fn a_container_named_in_a_way_broza_cannot_parse_is_skipped_with_a_warning() {
        let scenario = Scenario::new();
        let mut broken = container(vec![apfs_volume("disk3s1", "A", &["Data"])]);
        broken.container_reference = "not-a-disk".to_owned();
        let mut warnings: Vec<Warning> = Vec::new();

        let built = apfs_container(&broken, &scenario.inputs(), &mut warnings);

        assert!(built.is_none());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "unreadable_device_id");
    }

    #[test]
    fn a_volume_named_in_a_way_broza_cannot_parse_is_skipped_with_a_warning() {
        let scenario = Scenario::new();
        let mut volume = apfs_volume("disk3s1", "A", &["Data"]);
        volume.device_identifier = "sda1".to_owned();
        let mut warnings: Vec<Warning> = Vec::new();

        let built = apfs_container(&container(vec![volume]), &scenario.inputs(), &mut warnings)
            .unwrap_or_else(|| panic!("the container is still readable"));

        assert!(built.volumes.is_empty());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn purgeable_space_is_asked_of_the_data_volume_and_reported_once() {
        let list =
            DiskList { all_disks_and_partitions: vec![listed("disk3s5", Some("/System/Volumes/Data"))] };
        let scenario = Scenario::new().with_list(list).with_purgeable("/System/Volumes/Data", 4_096);

        let built = apfs_container(
            &container(vec![apfs_volume("disk3s5", "Data", &["Data"])]),
            &scenario.inputs(),
            &mut Vec::new(),
        )
        .unwrap_or_else(|| panic!("no container"));

        assert_eq!(built.purgeable_bytes, 4_096);
        assert_eq!(built.used_bytes, 600, "used is the ceiling minus what is free");
    }

    #[test]
    fn a_container_with_two_user_volumes_reports_the_shared_estimate_once() {
        let mut list =
            DiskList { all_disks_and_partitions: vec![listed("disk3s5", Some("/System/Volumes/Data"))] };
        list.all_disks_and_partitions[0].apfs_volumes.push(ListApfsVolume {
            device_identifier: "disk3s7".to_owned(),
            mount_point: Some(PathBuf::from("/Volumes/Extra")),
            ..ListApfsVolume::default()
        });
        let scenario = Scenario::new()
            .with_list(list)
            .with_infos(vec![info("disk3s7", Some("/Volumes/Extra"), true)])
            .with_purgeable("/System/Volumes/Data", 4_096)
            .with_purgeable("/Volumes/Extra", 4_096);

        let built = apfs_container(
            &container(vec![apfs_volume("disk3s5", "Data", &["Data"]), apfs_volume("disk3s7", "Extra", &[])]),
            &scenario.inputs(),
            &mut Vec::new(),
        )
        .unwrap_or_else(|| panic!("no container"));

        assert_eq!(built.volumes[1].role, VolumeRole::User);
        assert_eq!(built.purgeable_bytes, 4_096, "one shared pool, counted once");
    }

    /// Time Machine backing up to an APFS volume the user renamed.
    const RENAMED_DESTINATION: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
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
  </array>
</dict>
</plist>"#;

    #[test]
    fn a_writable_apfs_volume_time_machine_backs_up_to_is_a_backup_volume() {
        let list = DiskList { all_disks_and_partitions: vec![listed("disk5s1", Some("/Volumes/Backup4TB"))] };
        let scenario = Scenario::new()
            .with_list(list)
            .with_infos(vec![info("disk5s1", Some("/Volumes/Backup4TB"), true)])
            .with_destination_info(RENAMED_DESTINATION);

        let built = apfs_container(
            &container(vec![apfs_volume("disk5s1", "Backup4TB", &[])]),
            &scenario.inputs(),
            &mut Vec::new(),
        )
        .unwrap_or_else(|| panic!("no container"));

        assert_eq!(
            built.volumes[0].role,
            VolumeRole::Backup,
            "a roleless, writable volume under /Volumes would otherwise be the user's"
        );
        assert!(!built.volumes[0].writable_by_broza);
        assert_eq!(built.purgeable_bytes, 0, "a backup volume is not asked for purgeable space");
    }

    #[test]
    fn the_same_volume_without_time_machine_is_the_users() {
        let list = DiskList { all_disks_and_partitions: vec![listed("disk5s1", Some("/Volumes/Backup4TB"))] };
        let scenario = Scenario::new().with_list(list).with_infos(vec![info(
            "disk5s1",
            Some("/Volumes/Backup4TB"),
            true,
        )]);

        let built = apfs_container(
            &container(vec![apfs_volume("disk5s1", "Backup4TB", &[])]),
            &scenario.inputs(),
            &mut Vec::new(),
        )
        .unwrap_or_else(|| panic!("no container"));

        assert_eq!(built.volumes[0].role, VolumeRole::User);
    }

    #[test]
    fn a_data_volume_macos_mounted_read_only_is_not_writable_and_is_reported() {
        let list =
            DiskList { all_disks_and_partitions: vec![listed("disk3s5", Some("/System/Volumes/Data"))] };
        let scenario = Scenario::new().with_list(list).with_infos(vec![info(
            "disk3s5",
            Some("/System/Volumes/Data"),
            false,
        )]);
        let mut warnings: Vec<Warning> = Vec::new();

        let built = apfs_container(
            &container(vec![apfs_volume("disk3s5", "Data", &["Data"])]),
            &scenario.inputs(),
            &mut warnings,
        )
        .unwrap_or_else(|| panic!("no container"));

        assert_eq!(built.volumes[0].role, VolumeRole::Data, "the role is what macOS says it is");
        assert!(!built.volumes[0].writable_by_broza, "but Broza cannot write to it");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "data_volume_not_writable");
        assert!(warnings[0].message.contains("Data"), "{}", warnings[0].message);
    }

    #[test]
    fn a_data_volume_macos_calls_writable_stays_writable_without_a_warning() {
        let list =
            DiskList { all_disks_and_partitions: vec![listed("disk3s5", Some("/System/Volumes/Data"))] };
        let scenario = Scenario::new().with_list(list).with_infos(vec![info(
            "disk3s5",
            Some("/System/Volumes/Data"),
            true,
        )]);
        let mut warnings: Vec<Warning> = Vec::new();

        let built = apfs_container(
            &container(vec![apfs_volume("disk3s5", "Data", &["Data"])]),
            &scenario.inputs(),
            &mut warnings,
        )
        .unwrap_or_else(|| panic!("no container"));

        assert!(built.volumes[0].writable_by_broza);
        assert!(warnings.is_empty());
    }

    #[test]
    fn a_protected_volume_that_happens_to_be_writable_is_still_not_writable_by_broza() {
        let list = DiskList { all_disks_and_partitions: vec![listed("disk3s1", Some("/"))] };
        let scenario = Scenario::new().with_list(list).with_infos(vec![info("disk3s1", Some("/"), true)]);
        let mut warnings: Vec<Warning> = Vec::new();

        let built = apfs_container(
            &container(vec![apfs_volume("disk3s1", "Macintosh HD", &["System"])]),
            &scenario.inputs(),
            &mut warnings,
        )
        .unwrap_or_else(|| panic!("no container"));

        assert!(!built.volumes[0].writable_by_broza, "the role gate comes first");
        assert!(warnings.is_empty(), "a protected volume being read-only is not news");
    }

    #[test]
    fn a_container_without_a_writable_volume_reports_no_purgeable_space() {
        let scenario = Scenario::new().with_purgeable("/", 4_096);

        let built = apfs_container(
            &container(vec![apfs_volume("disk3s1", "Macintosh HD", &["System"])]),
            &scenario.inputs(),
            &mut Vec::new(),
        )
        .unwrap_or_else(|| panic!("no container"));

        assert_eq!(built.purgeable_bytes, 0);
    }

    #[test]
    fn a_roleless_apfs_volume_needs_the_writable_flag_to_become_the_users() {
        let list = DiskList { all_disks_and_partitions: vec![listed("disk5s1", Some("/Volumes/Ext"))] };
        let read_only = Scenario::new().with_list(list.clone()).with_infos(vec![info(
            "disk5s1",
            Some("/Volumes/Ext"),
            false,
        )]);
        let writable =
            Scenario::new().with_list(list).with_infos(vec![info("disk5s1", Some("/Volumes/Ext"), true)]);
        let volumes = vec![apfs_volume("disk5s1", "Ext", &[])];

        let without = apfs_container(&container(volumes.clone()), &read_only.inputs(), &mut Vec::new());
        let with = apfs_container(&container(volumes), &writable.inputs(), &mut Vec::new());

        assert_eq!(without.map(|c| c.volumes[0].role), Some(VolumeRole::Unknown));
        assert_eq!(with.map(|c| c.volumes[0].role), Some(VolumeRole::User));
    }

    #[test]
    fn a_sealed_system_volume_is_reported_at_the_root_but_judged_by_its_own_mount_point() {
        let mut list = DiskList {
            all_disks_and_partitions: vec![listed("disk3s1", Some("/System/Volumes/Update/mnt1"))],
        };
        list.all_disks_and_partitions[0].apfs_volumes[0].mounted_snapshots =
            vec![crate::adapters::diskutil::plist_list::ListMountedSnapshot {
                snapshot_name: Some("com.apple.os.update-abc".to_owned()),
                snapshot_bsd: Some("disk3s1s1".to_owned()),
                sealed: Some("Yes".to_owned()),
                snapshot_mount_point: Some(PathBuf::from("/")),
            }];
        let scenario = Scenario::new().with_list(list);

        let built = apfs_container(
            &container(vec![apfs_volume("disk3s1", "Macintosh HD", &["System"])]),
            &scenario.inputs(),
            &mut Vec::new(),
        )
        .unwrap_or_else(|| panic!("no container"));

        assert_eq!(built.volumes[0].mount_point.as_deref(), Some(Path::new("/")));
        assert_eq!(built.volumes[0].role, VolumeRole::System);
    }

    #[test]
    fn a_volume_nobody_reported_info_for_is_never_writable() {
        let list = DiskList { all_disks_and_partitions: vec![listed("disk5s1", Some("/Volumes/Ext"))] };
        let scenario = Scenario::new().with_list(list).with_infos(Vec::<DeviceInfo>::new());

        let built = apfs_container(
            &container(vec![apfs_volume("disk5s1", "Ext", &[])]),
            &scenario.inputs(),
            &mut Vec::new(),
        )
        .unwrap_or_else(|| panic!("no container"));

        assert_eq!(built.volumes[0].role, VolumeRole::Unknown);
    }
}
