//! Turning three `diskutil` outputs into the `disks[]` of the JSON contract.
//!
//! Pure: everything it needs is already parsed or behind a port, so the shape
//! of `docs/cli-spec.md` §4.2 can be tested without running a single process.
//! The division of labour is fixed by what each command actually reports —
//! `diskutil list` knows which devices exist and where volumes are mounted,
//! `diskutil apfs list` knows capacities and roles, `diskutil info` knows the
//! model of a disk, whether a volume is writable, and how full an `HFS+`
//! partition is.
//!
//! Nothing here fails. A container Broza cannot attribute, an identifier it
//! cannot parse, a capacity that contradicts itself: each becomes a warning and
//! the rest of the machine is still reported (`AGENTS.md` §6).

use super::devices::{nonzero_or, ordered_by_device, whole_disk_of};
use super::hfs::hfs_container;
use super::inputs::{Inputs, warning};
use super::plist_apfs::ApfsContainer;
use super::plist_list::ListDevice;
use super::volumes::{UNREADABLE_ID_CODE, apfs_container};
use crate::model::{Container, Disk, VolumeId, Warning};

/// Warning code for a container whose physical store is on no listed device.
const UNATTACHED_CONTAINER_CODE: &str = "unattached_container";
/// Warning code for a container that claims more space than it is built from.
const CAPACITY_MISMATCH_CODE: &str = "capacity_mismatch";

/// Build the disks of the JSON contract from the parsed `diskutil` output.
///
/// Devices, containers and volumes are ordered by BSD number, so two runs on an
/// unchanged machine produce byte-identical output.
pub(crate) fn assemble_disks(inputs: &Inputs<'_>, warnings: &mut Vec<Warning>) -> Vec<Disk> {
    let disks: Vec<Disk> =
        inputs.list.physical_devices().filter_map(|device| disk_for(device, inputs, warnings)).collect();
    report_unattached_containers(inputs, warnings);
    ordered_by_device(disks, |disk| disk.id.as_str())
}

/// One physical disk with everything carved out of it.
fn disk_for(device: &ListDevice, inputs: &Inputs<'_>, warnings: &mut Vec<Warning>) -> Option<Disk> {
    let Ok(id) = device.device_identifier.parse::<VolumeId>() else {
        warnings.push(warning(
            UNREADABLE_ID_CODE,
            format!("skipped a disk: `{}` is not a BSD device name", device.device_identifier),
            None,
        ));
        return None;
    };
    let info = inputs.infos.get(&device.device_identifier);
    let mut containers: Vec<Container> = Vec::new();
    for container in inputs.apfs.containers.iter().filter(|c| owns_container(&device.device_identifier, c)) {
        check_capacity(container, warnings);
        containers.extend(apfs_container(container, inputs, warnings));
    }
    for partition in device.hfs_partitions() {
        containers.extend(hfs_container(partition, inputs, warnings));
    }
    Some(Disk {
        id,
        model: info.and_then(|info| info.media_name.clone()).unwrap_or_default(),
        size_bytes: info.map_or(device.size, |info| nonzero_or(info.total_size, device.size)),
        internal: info.is_some_and(|info| info.internal),
        containers: ordered_by_device(containers, |container| container.id.as_str()),
    })
}

/// `true` when the largest physical store of `container` lives on `device`.
///
/// A fusion container spans several stores and belongs, for a reader trying to
/// find it in Disk Utility, to the one holding most of it. On a tie the last
/// of the equal stores wins, because that is what [`Iterator::max_by_key`]
/// returns; either answer is arbitrary and this one is at least the same on
/// every run.
fn owns_container(device: &str, container: &ApfsContainer) -> bool {
    container
        .physical_stores
        .iter()
        .max_by_key(|store| store.size)
        .is_some_and(|store| whole_disk_of(&store.device_identifier) == device)
}

/// Warn about containers no listed device claimed.
///
/// Such a container is real — it holds volumes and bytes — but Broza has no
/// disk to attach it to, and inventing one would report hardware that does not
/// exist. Leaving it silently out would understate the machine, so it is a
/// warning.
fn report_unattached_containers(inputs: &Inputs<'_>, warnings: &mut Vec<Warning>) {
    for container in inputs.apfs.containers.iter().filter(|c| is_unattached(inputs, c)) {
        warnings.push(warning(
            UNATTACHED_CONTAINER_CODE,
            format!(
                "container {} is not reported: its physical store {} belongs to no listed disk",
                container.container_reference,
                physical_store_name(container)
            ),
            None,
        ));
    }
}

/// `true` when no listed physical device owns `container`.
fn is_unattached(inputs: &Inputs<'_>, container: &ApfsContainer) -> bool {
    !inputs.list.physical_devices().any(|device| owns_container(&device.device_identifier, container))
}

/// The store a container is attributed by, for a warning message.
fn physical_store_name(container: &ApfsContainer) -> &str {
    container
        .physical_stores
        .iter()
        .max_by_key(|store| store.size)
        .map_or("(none)", |store| store.device_identifier.as_str())
}

/// Warn when a container claims more space than its physical stores hold.
///
/// Sizes are reported as `diskutil` gives them, never corrected: a number
/// Broza silently adjusts is a number nobody can check. Saying so is the honest
/// half of "honest numbers" (`AGENTS.md` §2.7).
fn check_capacity(container: &ApfsContainer, warnings: &mut Vec<Warning>) {
    let backing: u64 = container.physical_stores.iter().map(|store| store.size).sum();
    if backing == 0 || container.capacity_ceiling <= backing {
        return;
    }
    warnings.push(warning(
        CAPACITY_MISMATCH_CODE,
        format!(
            "container {} reports a capacity of {} bytes but is built from {} bytes of storage; \
             the reported figure is macOS's",
            container.container_reference, container.capacity_ceiling, backing
        ),
        None,
    ));
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{assemble_disks, owns_container};
    use crate::adapters::diskutil::plist_apfs::{ApfsContainer, ApfsList, ApfsPhysicalStore, ApfsVolume};
    use crate::adapters::diskutil::plist_info::DeviceInfo;
    use crate::adapters::diskutil::plist_list::{DiskList, ListDevice, ListPartition};
    use crate::adapters::diskutil::tests_support::Scenario;
    use crate::model::Warning;

    fn physical(id: &str, partitions: Vec<ListPartition>) -> ListDevice {
        ListDevice {
            device_identifier: id.to_owned(),
            content: "GUID_partition_scheme".to_owned(),
            size: 1_000,
            partitions,
            ..ListDevice::default()
        }
    }

    fn store(id: &str, size: u64) -> ApfsPhysicalStore {
        ApfsPhysicalStore { device_identifier: id.to_owned(), size }
    }

    fn container(reference: &str, stores: Vec<ApfsPhysicalStore>) -> ApfsContainer {
        ApfsContainer {
            container_reference: reference.to_owned(),
            capacity_ceiling: 1_000,
            capacity_free: 400,
            physical_stores: stores,
            volumes: vec![ApfsVolume {
                device_identifier: "disk3s1".to_owned(),
                name: Some("Macintosh HD".to_owned()),
                roles: vec!["System".to_owned()],
                ..ApfsVolume::default()
            }],
            ..ApfsContainer::default()
        }
    }

    fn one_disk(partitions: Vec<ListPartition>) -> DiskList {
        DiskList { all_disks_and_partitions: vec![physical("disk0", partitions)] }
    }

    #[test]
    fn a_container_belongs_to_the_disk_holding_most_of_it() {
        let fusion = container("disk3", vec![store("disk0s2", 100), store("disk1s2", 900)]);

        assert!(owns_container("disk1", &fusion));
        assert!(!owns_container("disk0", &fusion), "the smaller store does not claim it");
    }

    #[test]
    fn a_container_whose_disk_is_not_listed_is_reported_as_a_warning() {
        let scenario = Scenario::new()
            .with_list(one_disk(Vec::new()))
            .with_apfs(ApfsList { containers: vec![container("disk3", vec![store("disk99s1", 1_000)])] });
        let mut warnings: Vec<Warning> = Vec::new();

        let disks = assemble_disks(&scenario.inputs(), &mut warnings);

        assert_eq!(disks.len(), 1);
        assert!(disks[0].containers.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "unattached_container");
        assert!(warnings[0].message.contains("disk99s1"), "{}", warnings[0].message);
    }

    #[test]
    fn a_machine_whose_containers_all_found_a_disk_warns_about_nothing() {
        let scenario = Scenario::new()
            .with_list(one_disk(Vec::new()))
            .with_apfs(ApfsList { containers: vec![container("disk3", vec![store("disk0s2", 1_000)])] });
        let mut warnings: Vec<Warning> = Vec::new();

        let disks = assemble_disks(&scenario.inputs(), &mut warnings);

        assert_eq!(disks[0].containers.len(), 1);
        assert!(warnings.is_empty());
    }

    #[test]
    fn a_container_claiming_more_space_than_it_is_built_from_is_reported() {
        let scenario = Scenario::new()
            .with_list(one_disk(Vec::new()))
            .with_apfs(ApfsList { containers: vec![container("disk3", vec![store("disk0s2", 500)])] });
        let mut warnings: Vec<Warning> = Vec::new();

        let disks = assemble_disks(&scenario.inputs(), &mut warnings);

        assert_eq!(disks[0].containers[0].size_bytes, 1_000, "the figure is reported as macOS gave it");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "capacity_mismatch");
    }

    #[test]
    fn the_model_and_the_size_of_a_disk_come_from_diskutil_info() {
        let info = DeviceInfo {
            device_identifier: "disk0".to_owned(),
            media_name: Some("APPLE SSD".to_owned()),
            internal: true,
            total_size: 2_000,
            ..DeviceInfo::default()
        };
        let scenario = Scenario::new().with_list(one_disk(Vec::new())).with_infos(vec![info]);

        let disks = assemble_disks(&scenario.inputs(), &mut Vec::new());

        assert_eq!(disks[0].model, "APPLE SSD");
        assert_eq!(disks[0].size_bytes, 2_000);
        assert!(disks[0].internal);
    }

    #[test]
    fn a_disk_without_info_keeps_the_size_of_the_partition_map_and_no_model() {
        let scenario = Scenario::new().with_list(one_disk(Vec::new()));

        let disks = assemble_disks(&scenario.inputs(), &mut Vec::new());

        assert_eq!(disks[0].size_bytes, 1_000);
        assert_eq!(disks[0].model, "");
        assert!(!disks[0].internal);
    }

    #[test]
    fn a_disk_named_in_a_way_broza_cannot_parse_is_skipped_with_a_warning() {
        let scenario = Scenario::new()
            .with_list(DiskList { all_disks_and_partitions: vec![physical("hd0", Vec::new())] });
        let mut warnings: Vec<Warning> = Vec::new();

        let disks = assemble_disks(&scenario.inputs(), &mut warnings);

        assert!(disks.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "unreadable_device_id");
    }

    #[test]
    fn disks_and_their_contents_come_back_in_bsd_order() {
        let mut list = one_disk(vec![ListPartition {
            device_identifier: "disk0s9".to_owned(),
            content: "Apple_HFS".to_owned(),
            size: 10,
            volume_name: Some("Spare".to_owned()),
            mount_point: None,
        }]);
        list.all_disks_and_partitions.insert(0, physical("disk10", Vec::new()));
        let scenario = Scenario::new()
            .with_list(list)
            .with_apfs(ApfsList { containers: vec![container("disk3", vec![store("disk0s2", 1_000)])] });

        let disks = assemble_disks(&scenario.inputs(), &mut Vec::new());

        let ids: Vec<&str> = disks.iter().map(|disk| disk.id.as_str()).collect();
        assert_eq!(ids, vec!["disk0", "disk10"]);
        let containers: Vec<&str> =
            disks[0].containers.iter().map(|container| container.id.as_str()).collect();
        assert_eq!(containers, vec!["disk0s9", "disk3"]);
    }

    #[test]
    fn an_hfs_partition_becomes_a_container_of_its_own() {
        let partition = ListPartition {
            device_identifier: "disk0s3".to_owned(),
            content: "Apple_HFS".to_owned(),
            size: 500,
            volume_name: Some("Spare".to_owned()),
            mount_point: Some(PathBuf::from("/Volumes/Spare")),
        };
        let scenario = Scenario::new().with_list(one_disk(vec![partition]));

        let disks = assemble_disks(&scenario.inputs(), &mut Vec::new());

        assert_eq!(disks[0].containers.len(), 1);
        assert_eq!(disks[0].containers[0].volumes[0].name, "Spare");
    }
}
