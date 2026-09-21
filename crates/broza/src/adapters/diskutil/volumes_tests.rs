//! Tests for [`super::volumes`]: what each volume is, and which of them
//! Broza may write to.

use std::path::{Path, PathBuf};

use super::volumes::apfs_container;
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
    let list = DiskList { all_disks_and_partitions: vec![listed("disk3s5", Some("/System/Volumes/Data"))] };
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
    let scenario =
        Scenario::new().with_list(list).with_infos(vec![info("disk5s1", Some("/Volumes/Backup4TB"), true)]);

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
    let list = DiskList { all_disks_and_partitions: vec![listed("disk3s5", Some("/System/Volumes/Data"))] };
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
    let list = DiskList { all_disks_and_partitions: vec![listed("disk3s5", Some("/System/Volumes/Data"))] };
    let scenario =
        Scenario::new().with_list(list).with_infos(vec![info("disk3s5", Some("/System/Volumes/Data"), true)]);
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
    let mut list =
        DiskList { all_disks_and_partitions: vec![listed("disk3s1", Some("/System/Volumes/Update/mnt1"))] };
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
