//! An `HFS+` partition, reported as a container holding one volume.
//!
//! `HFS+` has no container layer, so the partition plays both parts. Its role
//! is decided by exactly the predicate an APFS volume with no role goes
//! through ([`super::roles::volume_role`]) and never by the filesystem family:
//! a mounted installer image is read-only and stays `unknown`, a Time Machine
//! destination is `backup`, and only a writable volume mounted directly under
//! `/Volumes` is the user's (`AGENTS.md` §2.3).

use super::devices::nonzero_or;
use super::inputs::{Inputs, warning};
use super::plist_list::ListPartition;
use super::purpose::purpose_for_volume;
use super::roles::volume_role;
use super::volumes::{UNREADABLE_ID_CODE, facts, purgeable_of, writable};
use crate::model::{Container, FsKind, Volume, VolumeId, Warning};

/// Bytes reported for a volume nothing could be measured on.
///
/// Not the same zero as "measured zero": an unmounted partition has a size but
/// no measurable usage, and reporting `0` used is the only honest placeholder
/// (`AGENTS.md` §2.7).
const UNMEASURED_BYTES: u64 = 0;

/// Build the container of one `HFS+` partition.
///
/// Usage comes from `diskutil info`, which only reports it while the volume is
/// mounted; an unmounted partition contributes its size and nothing else,
/// because a number Broza cannot measure is not a number it invents
/// (`AGENTS.md` §2.7).
pub(crate) fn hfs_container(
    partition: &ListPartition,
    inputs: &Inputs<'_>,
    warnings: &mut Vec<Warning>,
) -> Option<Container> {
    let Ok(id) = partition.device_identifier.parse::<VolumeId>() else {
        warnings.push(warning(
            UNREADABLE_ID_CODE,
            format!("skipped a partition: `{}` is not a BSD device name", partition.device_identifier),
            None,
        ));
        return None;
    };
    let info = inputs.infos.get(&partition.device_identifier);
    let mount_point =
        info.and_then(|info| info.mount_point.clone()).or_else(|| partition.mount_point.clone());
    let name = partition
        .volume_name
        .clone()
        .or_else(|| info.and_then(|info| info.volume_name.clone()))
        .unwrap_or_default();
    let uuid = info.and_then(|info| info.volume_uuid.clone());
    let observed = facts(info, &name, uuid.as_deref(), mount_point.as_deref(), inputs);
    let role = volume_role(&[], mount_point.as_deref(), observed);
    let size_bytes = info.map_or(partition.size, |info| nonzero_or(info.total_size, partition.size));
    let (used_bytes, free_bytes) = match (mount_point.as_ref(), info) {
        (Some(_), Some(info)) => (info.used_bytes(), info.free_space),
        _ => (UNMEASURED_BYTES, UNMEASURED_BYTES),
    };
    let volumes = vec![Volume {
        id: id.clone(),
        purpose: purpose_for_volume(role, &[], &name),
        writable_by_broza: writable(role, observed, &name, warnings),
        name,
        role,
        mount_point,
        used_bytes,
    }];
    Some(Container {
        id,
        kind: FsKind::HfsPlus,
        size_bytes,
        used_bytes,
        free_bytes,
        purgeable_bytes: purgeable_of(&volumes, inputs),
        volumes,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::hfs_container;
    use crate::adapters::diskutil::plist_info::DeviceInfo;
    use crate::adapters::diskutil::plist_list::ListPartition;
    use crate::adapters::diskutil::tests_support::Scenario;
    use crate::model::{FsKind, VolumeRole, Warning};

    fn partition(id: &str, name: &str, mount: Option<&str>) -> ListPartition {
        ListPartition {
            device_identifier: id.to_owned(),
            content: "Apple_HFS".to_owned(),
            size: 500,
            volume_name: Some(name.to_owned()),
            mount_point: mount.map(PathBuf::from),
        }
    }

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

    #[test]
    fn a_read_only_disk_image_is_not_a_user_volume() {
        let scenario = Scenario::new().with_infos(vec![info("disk4s1", Some("/Volumes/Installer"), false)]);

        let built = hfs_container(
            &partition("disk4s1", "Installer", Some("/Volumes/Installer")),
            &scenario.inputs(),
            &mut Vec::new(),
        )
        .unwrap_or_else(|| panic!("no container"));

        assert_eq!(built.kind, FsKind::HfsPlus);
        assert_eq!(built.volumes[0].role, VolumeRole::Unknown);
        assert!(!built.volumes[0].writable_by_broza, "a read-only image is never writable");
        assert_eq!((built.size_bytes, built.used_bytes, built.free_bytes), (1_000, 750, 250));
    }

    #[test]
    fn a_writable_hfs_volume_under_volumes_is_the_users() {
        let scenario = Scenario::new().with_infos(vec![info("disk4s1", Some("/Volumes/Scratch"), true)]);

        let built = hfs_container(
            &partition("disk4s1", "Scratch", Some("/Volumes/Scratch")),
            &scenario.inputs(),
            &mut Vec::new(),
        )
        .unwrap_or_else(|| panic!("no container"));

        assert_eq!(built.volumes[0].role, VolumeRole::User);
        assert!(built.volumes[0].writable_by_broza);
    }

    #[test]
    fn a_time_machine_destination_is_a_backup_volume_even_when_writable() {
        let scenario = Scenario::new()
            .with_infos(vec![info("disk4s1", Some("/Volumes/TM"), true)])
            .with_backup_marker("/Volumes/TM");

        let built = hfs_container(
            &partition("disk4s1", "TM", Some("/Volumes/TM")),
            &scenario.inputs(),
            &mut Vec::new(),
        )
        .unwrap_or_else(|| panic!("no container"));

        assert_eq!(built.volumes[0].role, VolumeRole::Backup);
        assert!(!built.volumes[0].writable_by_broza, "backups are only ever touched through tmutil");
    }

    #[test]
    fn a_volume_named_after_time_machine_is_a_backup_volume_too() {
        let scenario = Scenario::new().with_infos(vec![info("disk4s1", Some("/Volumes/TM"), true)]);

        let built = hfs_container(
            &partition("disk4s1", "Time Machine Backups", Some("/Volumes/TM")),
            &scenario.inputs(),
            &mut Vec::new(),
        )
        .unwrap_or_else(|| panic!("no container"));

        assert_eq!(built.volumes[0].role, VolumeRole::Backup);
    }

    #[test]
    fn an_unmounted_hfs_partition_reports_its_size_and_no_usage() {
        let scenario = Scenario::new();

        let built = hfs_container(&partition("disk4s1", "Spare", None), &scenario.inputs(), &mut Vec::new())
            .unwrap_or_else(|| panic!("no container"));

        assert_eq!((built.size_bytes, built.used_bytes, built.free_bytes), (500, 0, 0));
        assert_eq!(built.volumes[0].role, VolumeRole::Unknown);
        assert_eq!(built.volumes[0].mount_point, None);
    }

    #[test]
    fn a_partition_named_in_a_way_broza_cannot_parse_is_skipped_with_a_warning() {
        let scenario = Scenario::new();
        let mut warnings: Vec<Warning> = Vec::new();
        let mut broken = partition("disk4s1", "X", None);
        broken.device_identifier = "hd0".to_owned();

        assert!(hfs_container(&broken, &scenario.inputs(), &mut warnings).is_none());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "unreadable_device_id");
    }
}
