//! `DiskutilEnumerator` end to end against recorded macOS 26 output.
//!
//! The runner is a `FakeRunner` scripted with the fixtures
//! `scripts/capture-diskutil-fixtures.sh` recorded, so this exercises the whole
//! path — three kinds of command, four parsers, the role mapping, the purpose
//! text and the assembly — without touching a disk (`AGENTS.md` §7).
//!
//! The snapshot is the `disks[]` of `docs/cli-spec.md` §4.2 as JSON: it is the
//! contract, so a change in it has to be reviewed as a change in the contract.
//!
//! Only the `test-support` feature exposes `broza::testing`; without it this file
//! compiles to nothing.
#![cfg(feature = "test-support")]

use std::sync::Arc;

use broza::BrozaError;
use broza::adapters::diskutil::{DISKUTIL, DiskutilEnumerator};
use broza::adapters::tmutil_destinations::{DESTINATION_INFO_ARGS, TMUTIL};
use broza::model::{FsKind, VolumeRole};
use broza::ports::{DiskEnumerator, EnumerationReport, FileOps, ProcessRunner, SpaceProvider};
use broza::testing::{FakeFileOps, FakeRunner, FakeSpace};

/// Fixture directory of the macOS major this test replays.
const MACOS_MAJOR: &str = "macos26";
/// BSD name of the data volume in the recorded machine.
const DATA_VOLUME: &str = "disk3s5";
/// Mount point of the data volume in the recorded machine.
const DATA_MOUNT_POINT: &str = "/System/Volumes/Data";
/// Purgeable bytes the space provider reports for the data volume.
const PURGEABLE_BYTES: u64 = 84_140_000_000;
/// Physical disks in the recording: the internal SSD and five disk images.
const PHYSICAL_DISKS: [&str; 6] = ["disk0", "disk4", "disk5", "disk6", "disk7", "disk8"];
/// `HFS+` partitions in the recording, one per mounted disk image.
const HFS_PARTITIONS: [&str; 5] = ["disk4s1", "disk5s2", "disk6s1", "disk7s1", "disk8s1"];

/// A runner answering every command the enumerator issues on this machine.
fn recorded_runner() -> FakeRunner {
    recorded_runner_with_destinations("tmutil_destinationinfo.plist")
}

/// The same machine, with Time Machine answering out of `destination_fixture`.
fn recorded_runner_with_destinations(destination_fixture: &str) -> FakeRunner {
    let fixture = |name: &str| format!("plist/{MACOS_MAJOR}/{name}");
    let mut runner = FakeRunner::new()
        .with_fixture(DISKUTIL, &["list", "-plist"], &fixture("list.plist"))
        .and_then(|runner| {
            runner.with_fixture(DISKUTIL, &["apfs", "list", "-plist"], &fixture("apfs_list.plist"))
        })
        .and_then(|runner| runner.with_fixture(TMUTIL, &DESTINATION_INFO_ARGS, &fixture(destination_fixture)))
        .and_then(|runner| {
            // `diskutil info -plist /System/Volumes/Data` was recorded for the
            // data volume, which the enumerator asks for by its BSD name.
            runner.with_fixture(DISKUTIL, &["info", "-plist", DATA_VOLUME], &fixture("info_data.plist"))
        })
        .unwrap_or_else(|error| panic!("{error}"));
    for device in PHYSICAL_DISKS.iter().chain(HFS_PARTITIONS.iter()) {
        runner = runner
            .with_fixture(DISKUTIL, &["info", "-plist", device], &fixture(&format!("info_{device}.plist")))
            .unwrap_or_else(|error| panic!("{error}"));
    }
    runner
}

fn enumerator(runner: FakeRunner) -> DiskutilEnumerator {
    let space = FakeSpace::new().with_purgeable(DATA_MOUNT_POINT, PURGEABLE_BYTES);
    DiskutilEnumerator::new(
        Arc::new(runner) as Arc<dyn ProcessRunner>,
        Arc::new(space) as Arc<dyn SpaceProvider>,
        Arc::new(FakeFileOps::new()) as Arc<dyn FileOps>,
    )
}

fn recorded_report() -> EnumerationReport {
    enumerator(recorded_runner()).enumerate().unwrap_or_else(|error| panic!("{error}"))
}

fn recorded_disks() -> Vec<broza::model::Disk> {
    recorded_report().disks
}

#[test]
fn a_recorded_mac_is_enumerated_exactly_as_the_json_contract_describes() {
    insta::assert_json_snapshot!(recorded_disks());
}

#[test]
fn the_internal_disk_carries_the_boot_container_and_the_images_carry_hfs_plus() {
    let disks = recorded_disks();

    let ids: Vec<&str> = disks.iter().map(|disk| disk.id.as_str()).collect();
    assert_eq!(ids, PHYSICAL_DISKS.to_vec(), "physical disks only, in BSD order");
    let internal = &disks[0];
    assert!(internal.internal);
    assert_eq!(internal.model, "APPLE SSD AP0512Z");
    assert!(internal.containers.iter().all(|container| container.kind == FsKind::Apfs));
    assert!(disks[1].containers.iter().all(|container| container.kind == FsKind::HfsPlus));
    assert!(!disks[1].internal, "a disk image is not internal storage");
}

#[test]
fn the_boot_container_reports_every_role_of_a_modern_macos_install() {
    let disks = recorded_disks();
    let boot = disks[0]
        .containers
        .iter()
        .find(|container| container.id.as_str() == "disk3")
        .unwrap_or_else(|| panic!("boot container missing"));

    let roles: Vec<VolumeRole> = boot.volumes.iter().map(|volume| volume.role).collect();
    assert_eq!(
        roles,
        vec![
            VolumeRole::System,
            VolumeRole::Preboot,
            VolumeRole::Recovery,
            VolumeRole::Unknown,
            VolumeRole::Data,
            VolumeRole::Vm,
        ],
        "the Update volume has a role Broza does not model and must stay unknown"
    );
    assert_eq!(boot.volumes.iter().filter(|volume| volume.writable_by_broza).count(), 1);
}

#[test]
fn the_system_volume_is_reported_at_the_root_and_never_writable() {
    let disks = recorded_disks();
    let system = disks
        .iter()
        .flat_map(|disk| &disk.containers)
        .flat_map(|container| &container.volumes)
        .find(|volume| volume.role == VolumeRole::System)
        .unwrap_or_else(|| panic!("no system volume"));

    assert_eq!(system.mount_point.as_deref(), Some(std::path::Path::new("/")));
    assert!(!system.writable_by_broza);
    assert!(system.purpose.contains("sealed"));
}

#[test]
fn unmounted_volumes_are_enumerated_without_a_mount_point() {
    let disks = recorded_disks();
    let recovery: Vec<&str> = disks
        .iter()
        .flat_map(|disk| &disk.containers)
        .flat_map(|container| &container.volumes)
        .filter(|volume| volume.role == VolumeRole::Recovery && volume.mount_point.is_none())
        .map(|volume| volume.id.as_str())
        .collect();

    assert!(!recovery.is_empty(), "the recovery volumes of this machine are not mounted");
}

#[test]
fn purgeable_space_lands_on_the_container_of_the_data_volume_and_nowhere_else() {
    let disks = recorded_disks();
    let containers: Vec<(&str, u64)> = disks
        .iter()
        .flat_map(|disk| &disk.containers)
        .map(|container| (container.id.as_str(), container.purgeable_bytes))
        .collect();

    let with_purgeable: Vec<&(&str, u64)> =
        containers.iter().filter(|(_, purgeable)| *purgeable > 0).collect();
    assert_eq!(with_purgeable, vec![&("disk3", PURGEABLE_BYTES)]);
}

#[test]
fn free_space_and_purgeable_space_are_never_added_together() {
    let disks = recorded_disks();
    let boot = disks[0]
        .containers
        .iter()
        .find(|container| container.id.as_str() == "disk3")
        .unwrap_or_else(|| panic!("boot container missing"));

    assert_eq!(boot.used_bytes + boot.free_bytes, boot.size_bytes);
    assert!(boot.purgeable_bytes > 0, "the estimate is reported on its own");
}

#[test]
fn a_mounted_disk_image_reports_the_space_diskutil_info_measured() {
    let disks = recorded_disks();
    let image = &disks[1].containers[0];

    assert_eq!(image.used_bytes + image.free_bytes, image.size_bytes);
    assert_eq!(image.volumes.len(), 1);
    assert!(image.volumes[0].mount_point.is_some());
}

#[test]
fn a_read_only_installer_image_is_never_writable_however_it_is_mounted() {
    let disks = recorded_disks();

    let images: Vec<&broza::model::Volume> = disks[1..]
        .iter()
        .flat_map(|disk| &disk.containers)
        .flat_map(|container| &container.volumes)
        .collect();

    assert!(!images.is_empty(), "the recording has mounted disk images");
    for volume in images {
        assert_eq!(volume.role, VolumeRole::Unknown, "{}: WritableVolume is false", volume.id);
        assert!(!volume.writable_by_broza, "{}", volume.id);
    }
}

#[test]
fn every_writable_volume_of_the_recorded_machine_is_the_data_volume_and_nothing_else() {
    let disks = recorded_disks();

    let writable: Vec<&str> = disks
        .iter()
        .flat_map(|disk| &disk.containers)
        .flat_map(|container| &container.volumes)
        .filter(|volume| volume.writable_by_broza)
        .map(|volume| volume.id.as_str())
        .collect();

    assert_eq!(writable, vec!["disk3s5"]);
}

#[test]
fn a_recording_broza_understands_end_to_end_produces_no_warnings() {
    assert!(recorded_report().warnings.is_empty());
}

#[test]
fn enumerating_twice_produces_the_same_answer() {
    assert_eq!(recorded_disks(), recorded_disks());
}

#[test]
fn a_command_the_machine_refuses_fails_the_enumeration() {
    let runner = FakeRunner::new()
        .with_fixture(DISKUTIL, &["list", "-plist"], &format!("plist/{MACOS_MAJOR}/list.plist"))
        .unwrap_or_else(|error| panic!("{error}"));

    let err = enumerator(runner).enumerate().err();

    assert!(matches!(err, Some(BrozaError::Other(_))), "{err:?}");
}
