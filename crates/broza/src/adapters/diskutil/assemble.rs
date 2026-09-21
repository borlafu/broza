//! Turning three `diskutil` outputs into the `disks[]` of the JSON contract.
//!
//! Pure: everything it needs is already parsed, so the shape of
//! `docs/cli-spec.md` §4.2 can be tested without running a single process. The
//! division of labour is fixed by what each command actually reports —
//! `diskutil list` knows which devices exist and where volumes are mounted,
//! `diskutil apfs list` knows capacities and roles, `diskutil info` knows the
//! model of a disk and the free space of an `HFS+` partition.

use std::collections::BTreeMap;

use super::plist_apfs::{ApfsContainer, ApfsList};
use super::plist_info::DeviceInfo;
use super::plist_list::{DiskList, ListDevice, ListPartition};
use super::purpose::purpose_for;
use super::roles::volume_role;
use crate::model::{Container, Disk, FsKind, Volume, VolumeId, VolumeRole};
use crate::ports::SpaceProvider;

/// Purgeable bytes reported when macOS will not answer.
const UNKNOWN_PURGEABLE_BYTES: u64 = 0;

/// `diskutil info` output, keyed by BSD identifier.
pub(crate) type InfoByDevice = BTreeMap<String, DeviceInfo>;

/// Build the disks of the JSON contract from the parsed `diskutil` output.
///
/// Devices, containers and volumes are ordered by BSD number, so two runs on an
/// unchanged machine produce byte-identical output. A container whose physical
/// store belongs to no listed device is left out: Broza has nowhere to attach it
/// and inventing a disk for it would report hardware that does not exist.
pub(crate) fn assemble_disks(
    list: &DiskList,
    apfs: &ApfsList,
    infos: &InfoByDevice,
    space: &dyn SpaceProvider,
) -> Vec<Disk> {
    let mut disks: Vec<Disk> =
        list.physical_devices().filter_map(|device| disk_for(device, list, apfs, infos, space)).collect();
    disks.sort_by_key(|entry| device_order(entry.id.as_str()));
    disks
}

/// One physical disk with everything carved out of it.
fn disk_for(
    device: &ListDevice,
    list: &DiskList,
    apfs: &ApfsList,
    infos: &InfoByDevice,
    space: &dyn SpaceProvider,
) -> Option<Disk> {
    let id: VolumeId = device.device_identifier.parse().ok()?;
    let info = infos.get(&device.device_identifier);
    let mut containers: Vec<Container> = apfs
        .containers
        .iter()
        .filter(|container| owns_container(&device.device_identifier, container))
        .map(|container| apfs_container(container, list, space))
        .chain(device.hfs_partitions().filter_map(|partition| hfs_container(partition, infos)))
        .collect();
    containers.sort_by_key(|entry| device_order(entry.id.as_str()));
    Some(Disk {
        id,
        model: info.and_then(|info| info.media_name.clone()).unwrap_or_default(),
        size_bytes: info.map_or(device.size, |info| nonzero_or(info.total_size, device.size)),
        internal: info.is_some_and(|info| info.internal),
        containers,
    })
}

/// `true` when the first physical store of `container` lives on `device`.
fn owns_container(device: &str, container: &ApfsContainer) -> bool {
    container.physical_stores.first().is_some_and(|store| whole_disk_of(&store.device_identifier) == device)
}

/// One APFS container with its volumes.
fn apfs_container(container: &ApfsContainer, list: &DiskList, space: &dyn SpaceProvider) -> Container {
    let volumes: Vec<Volume> = ordered(container.volumes.iter().map(|volume| {
        let list_volume = list.apfs_volume(&volume.device_identifier);
        let mount_point = list_volume.and_then(super::plist_list::ListApfsVolume::effective_mount_point);
        let role = volume_role(&volume.roles, mount_point.as_deref());
        let name = volume
            .name
            .clone()
            .or_else(|| list_volume.and_then(|listed| listed.volume_name.clone()))
            .unwrap_or_default();
        Some(Volume {
            id: volume.device_identifier.parse().ok()?,
            purpose: purpose_for(role, &name),
            name,
            role,
            mount_point,
            used_bytes: volume.capacity_in_use,
            writable_by_broza: role.writable_by_broza(),
        })
    }));
    Container {
        id: container.container_reference.parse().unwrap_or_else(|_| fallback_id()),
        kind: FsKind::Apfs,
        size_bytes: container.capacity_ceiling,
        used_bytes: container.used_bytes(),
        free_bytes: container.capacity_free,
        purgeable_bytes: purgeable_of(&volumes, space),
        volumes,
    }
}

/// An `HFS+` partition, reported as a container holding one volume.
///
/// `HFS+` has no container layer, so the partition plays both parts. Usage comes
/// from `diskutil info`, which only reports it while the volume is mounted; an
/// unmounted partition contributes its size and nothing else, because a number
/// Broza cannot measure is not a number it invents (`AGENTS.md` §2.7).
fn hfs_container(partition: &ListPartition, infos: &InfoByDevice) -> Option<Container> {
    let id: VolumeId = partition.device_identifier.parse().ok()?;
    let info = infos.get(&partition.device_identifier);
    let mount_point =
        info.and_then(|info| info.mount_point.clone()).or_else(|| partition.mount_point.clone());
    let size_bytes = info.map_or(partition.size, |info| nonzero_or(info.total_size, partition.size));
    let (used_bytes, free_bytes) = match (mount_point.as_ref(), info) {
        (Some(_), Some(info)) => (info.used_bytes(), info.free_space),
        _ => (0, 0),
    };
    let name = partition.volume_name.clone().unwrap_or_default();
    Some(Container {
        id: id.clone(),
        kind: FsKind::HfsPlus,
        size_bytes,
        used_bytes,
        free_bytes,
        purgeable_bytes: UNKNOWN_PURGEABLE_BYTES,
        volumes: vec![Volume {
            id,
            purpose: purpose_for(VolumeRole::User, &name),
            name,
            role: VolumeRole::User,
            mount_point,
            used_bytes,
            writable_by_broza: VolumeRole::User.writable_by_broza(),
        }],
    })
}

/// Purgeable bytes of a container: the estimate for its data volume.
///
/// The number belongs to a mount point, and inside an APFS container only the
/// data volume has one worth asking about. A container without one, or a
/// mount point macOS refuses to answer for, reports zero: purgeable space is an
/// estimate and an estimate nobody made is not a guess Broza invents.
fn purgeable_of(volumes: &[Volume], space: &dyn SpaceProvider) -> u64 {
    volumes
        .iter()
        .filter(|volume| volume.role == VolumeRole::Data)
        .find_map(|volume| volume.mount_point.as_ref())
        .map_or(UNKNOWN_PURGEABLE_BYTES, |mount_point| {
            space.purgeable_bytes(mount_point).unwrap_or(UNKNOWN_PURGEABLE_BYTES)
        })
}

/// Collect volumes that could be identified, in BSD order.
fn ordered(volumes: impl Iterator<Item = Option<Volume>>) -> Vec<Volume> {
    let mut volumes: Vec<Volume> = volumes.flatten().collect();
    volumes.sort_by_key(|entry| device_order(entry.id.as_str()));
    volumes
}

/// The whole disk a partition belongs to: `disk0s2` is on `disk0`.
fn whole_disk_of(partition: &str) -> &str {
    let Some(rest) = partition.strip_prefix("disk") else { return partition };
    match rest.find('s') {
        Some(offset) => &partition[.."disk".len() + offset],
        None => partition,
    }
}

/// Sort key that orders `disk9` before `disk10`, unlike a string comparison.
fn device_order(id: &str) -> Vec<u64> {
    id.split(|character: char| !character.is_ascii_digit())
        .filter_map(|part| part.parse::<u64>().ok())
        .collect()
}

/// `value` unless it is zero, in which case `fallback`.
fn nonzero_or(value: u64, fallback: u64) -> u64 {
    if value == 0 { fallback } else { value }
}

/// Identifier used for a container `diskutil` named in a way Broza cannot parse.
///
/// Unreachable with real output — `ContainerReference` is always a BSD name —
/// but the contract needs a value, and `disk0` is the one identifier that always
/// exists.
fn fallback_id() -> VolumeId {
    VolumeId::try_from("disk0").unwrap_or_else(|_| unreachable!("disk0 is a valid BSD name"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use super::{InfoByDevice, assemble_disks, device_order, hfs_container, nonzero_or, whole_disk_of};
    use crate::adapters::diskutil::plist_apfs::{ApfsContainer, ApfsList, ApfsPhysicalStore, ApfsVolume};
    use crate::adapters::diskutil::plist_info::DeviceInfo;
    use crate::adapters::diskutil::plist_list::{DiskList, ListDevice, ListPartition};
    use crate::model::{FsKind, VolumeRole};
    use crate::testing::FakeSpace;

    fn physical(id: &str, partitions: Vec<ListPartition>) -> ListDevice {
        ListDevice {
            device_identifier: id.to_owned(),
            content: "GUID_partition_scheme".to_owned(),
            size: 1_000,
            partitions,
            ..ListDevice::default()
        }
    }

    fn hfs_partition(id: &str, name: &str, mount: Option<&str>) -> ListPartition {
        ListPartition {
            device_identifier: id.to_owned(),
            content: "Apple_HFS".to_owned(),
            size: 500,
            volume_name: Some(name.to_owned()),
            mount_point: mount.map(PathBuf::from),
        }
    }

    fn container(reference: &str, store: &str, volumes: Vec<ApfsVolume>) -> ApfsContainer {
        ApfsContainer {
            container_reference: reference.to_owned(),
            capacity_ceiling: 1_000,
            capacity_free: 400,
            physical_stores: vec![ApfsPhysicalStore { device_identifier: store.to_owned(), size: 1_000 }],
            volumes,
            ..ApfsContainer::default()
        }
    }

    fn apfs_volume(id: &str, name: &str, role: &str) -> ApfsVolume {
        ApfsVolume {
            device_identifier: id.to_owned(),
            name: Some(name.to_owned()),
            roles: vec![role.to_owned()],
            capacity_in_use: 100,
            ..ApfsVolume::default()
        }
    }

    fn infos(entries: Vec<DeviceInfo>) -> InfoByDevice {
        entries.into_iter().map(|info| (info.device_identifier.clone(), info)).collect()
    }

    #[test]
    fn a_partition_belongs_to_the_disk_its_name_starts_with() {
        assert_eq!(whole_disk_of("disk0s2"), "disk0");
        assert_eq!(whole_disk_of("disk12s3s1"), "disk12");
        assert_eq!(whole_disk_of("disk3"), "disk3");
        assert_eq!(whole_disk_of("nvme0n1"), "nvme0n1", "a name Broza cannot split stays whole");
    }

    #[test]
    fn devices_are_ordered_by_number_and_not_alphabetically() {
        assert!(device_order("disk9") < device_order("disk10"));
        assert!(device_order("disk3s2") < device_order("disk3s10"));
        assert!(device_order("disk3") < device_order("disk3s1"));
    }

    #[test]
    fn a_zero_size_falls_back_to_what_the_partition_map_reported() {
        assert_eq!(nonzero_or(0, 7), 7);
        assert_eq!(nonzero_or(5, 7), 5);
    }

    #[test]
    fn a_container_whose_disk_is_not_listed_is_left_out() {
        let list = DiskList { all_disks_and_partitions: vec![physical("disk0", vec![])] };
        let apfs = ApfsList {
            containers: vec![container("disk3", "disk99s1", vec![apfs_volume("disk3s1", "A", "Data")])],
        };

        let disks = assemble_disks(&list, &apfs, &BTreeMap::new(), &FakeSpace::new());

        assert_eq!(disks.len(), 1);
        assert!(disks[0].containers.is_empty());
    }

    #[test]
    fn purgeable_space_is_asked_for_the_data_volume_only() {
        let list = DiskList { all_disks_and_partitions: vec![physical("disk0", vec![])] };
        let mut data = apfs_volume("disk3s5", "Data", "Data");
        data.capacity_in_use = 300;
        let apfs = ApfsList {
            containers: vec![container(
                "disk3",
                "disk0s2",
                vec![apfs_volume("disk3s1", "Macintosh HD", "System"), data],
            )],
        };
        let mut listed = list.clone();
        listed.all_disks_and_partitions.push(ListDevice {
            device_identifier: "disk3".to_owned(),
            content: "Apple_APFS_Container".to_owned(),
            apfs_volumes: vec![crate::adapters::diskutil::plist_list::ListApfsVolume {
                device_identifier: "disk3s5".to_owned(),
                mount_point: Some(PathBuf::from("/System/Volumes/Data")),
                ..crate::adapters::diskutil::plist_list::ListApfsVolume::default()
            }],
            ..ListDevice::default()
        });
        let space = FakeSpace::new().with_purgeable("/System/Volumes/Data", 4_096);

        let disks = assemble_disks(&listed, &apfs, &BTreeMap::new(), &space);

        let container = &disks[0].containers[0];
        assert_eq!(container.purgeable_bytes, 4_096);
        assert_eq!(container.used_bytes, 600, "used is the ceiling minus what is free");
        assert_eq!(container.volumes[1].role, VolumeRole::Data);
    }

    #[test]
    fn a_container_without_a_mounted_data_volume_reports_no_purgeable_space() {
        let list = DiskList { all_disks_and_partitions: vec![physical("disk0", vec![])] };
        let apfs = ApfsList {
            containers: vec![container(
                "disk3",
                "disk0s2",
                vec![apfs_volume("disk3s1", "Macintosh HD", "System")],
            )],
        };

        let disks = assemble_disks(&list, &apfs, &BTreeMap::new(), &FakeSpace::new());

        assert_eq!(disks[0].containers[0].purgeable_bytes, 0);
    }

    #[test]
    fn a_mounted_hfs_partition_reports_the_usage_diskutil_info_measured() {
        let partition = hfs_partition("disk4s1", "Installer", Some("/Volumes/Installer"));
        let info = DeviceInfo {
            device_identifier: "disk4s1".to_owned(),
            total_size: 1_000,
            free_space: 250,
            mount_point: Some(PathBuf::from("/Volumes/Installer")),
            ..DeviceInfo::default()
        };

        let built = hfs_container(&partition, &infos(vec![info])).unwrap_or_else(|| panic!("no container"));

        assert_eq!(built.kind, FsKind::HfsPlus);
        assert_eq!((built.size_bytes, built.used_bytes, built.free_bytes), (1_000, 750, 250));
        assert_eq!(built.volumes[0].role, VolumeRole::User);
        assert!(built.volumes[0].writable_by_broza);
        assert_eq!(built.volumes[0].mount_point.as_deref(), Some(Path::new("/Volumes/Installer")));
    }

    #[test]
    fn an_unmounted_hfs_partition_reports_its_size_and_no_usage() {
        let partition = hfs_partition("disk4s1", "Spare", None);

        let built = hfs_container(&partition, &BTreeMap::new()).unwrap_or_else(|| panic!("no container"));

        assert_eq!((built.size_bytes, built.used_bytes, built.free_bytes), (500, 0, 0));
        assert_eq!(built.volumes[0].mount_point, None);
        assert!(built.volumes[0].purpose.contains("Spare"));
    }

    #[test]
    fn the_model_and_the_size_of_a_disk_come_from_diskutil_info() {
        let list = DiskList {
            all_disks_and_partitions: vec![physical("disk0", vec![hfs_partition("disk0s1", "X", None)])],
        };
        let info = DeviceInfo {
            device_identifier: "disk0".to_owned(),
            media_name: Some("APPLE SSD".to_owned()),
            internal: true,
            total_size: 2_000,
            ..DeviceInfo::default()
        };

        let disks = assemble_disks(&list, &ApfsList::default(), &infos(vec![info]), &FakeSpace::new());

        assert_eq!(disks[0].model, "APPLE SSD");
        assert_eq!(disks[0].size_bytes, 2_000);
        assert!(disks[0].internal);
    }

    #[test]
    fn a_disk_without_info_keeps_the_size_of_the_partition_map() {
        let list = DiskList { all_disks_and_partitions: vec![physical("disk0", vec![])] };

        let disks = assemble_disks(&list, &ApfsList::default(), &BTreeMap::new(), &FakeSpace::new());

        assert_eq!(disks[0].size_bytes, 1_000);
        assert_eq!(disks[0].model, "");
        assert!(!disks[0].internal);
    }
}
