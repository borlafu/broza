//! A mount table shaped like a real Apple Silicon Mac.
//!
//! One APFS container holding the sealed System volume, its Data sibling with the
//! firmlinks that make `/Users` live on it, the VM, Preboot and Recovery volumes, and
//! one external disk. Every role Broza protects (`AGENTS.md` §2.3) appears exactly
//! once, so a role or firmlink test can use this instead of inventing a tree.

use std::path::PathBuf;

use crate::model::{Volume, VolumeId, VolumeRole};
use crate::scan::{MountEntry, MountTable};

/// Device of the System volume.
const SYSTEM_DEVICE: u64 = 1;
/// Device of the Data volume.
const DATA_DEVICE: u64 = 2;
/// Device of the VM volume.
const VM_DEVICE: u64 = 3;
/// Device of the Preboot volume.
const PREBOOT_DEVICE: u64 = 4;
/// Device of the Recovery volume.
const RECOVERY_DEVICE: u64 = 5;
/// Device of the external volume.
const EXTERNAL_DEVICE: u64 = 6;
/// Firmlinks the Data volume is reached through.
const DATA_FIRMLINKS: [&str; 4] = ["/Users", "/Applications", "/Library", "/private"];

/// One volume of the fixture: role, mount point, device, and whether it is mounted.
struct Fixture {
    /// BSD identifier.
    id: &'static str,
    /// Volume name as Finder shows it.
    name: &'static str,
    /// Role assigned by macOS.
    role: VolumeRole,
    /// Where the volume sits in the filesystem.
    mount_point: &'static str,
    /// Device id of that mount point.
    device: u64,
    /// `false` for the volumes macOS keeps hidden and unmounted.
    mounted: bool,
    /// One-sentence explanation of what the volume is for.
    purpose: &'static str,
}

/// The fixture, in the order `diskutil` reports it.
const FIXTURES: [Fixture; 6] = [
    Fixture {
        id: "disk3s1",
        name: "Macintosh HD",
        role: VolumeRole::System,
        mount_point: "/",
        device: SYSTEM_DEVICE,
        mounted: true,
        purpose: "The sealed, read-only macOS system volume.",
    },
    Fixture {
        id: "disk3s5",
        name: "Macintosh HD - Data",
        role: VolumeRole::Data,
        mount_point: "/System/Volumes/Data",
        device: DATA_DEVICE,
        mounted: true,
        purpose: "Your files, applications and settings.",
    },
    Fixture {
        id: "disk3s6",
        name: "VM",
        role: VolumeRole::Vm,
        mount_point: "/System/Volumes/VM",
        device: VM_DEVICE,
        mounted: true,
        purpose: "Virtual memory swap files, managed by macOS.",
    },
    Fixture {
        id: "disk3s2",
        name: "Preboot",
        role: VolumeRole::Preboot,
        mount_point: "/System/Volumes/Preboot",
        device: PREBOOT_DEVICE,
        mounted: false,
        purpose: "Boot loader data needed before macOS starts.",
    },
    Fixture {
        id: "disk3s3",
        name: "Recovery",
        role: VolumeRole::Recovery,
        mount_point: "/System/Volumes/Recovery",
        device: RECOVERY_DEVICE,
        mounted: false,
        purpose: "The recovery environment used to reinstall macOS.",
    },
    Fixture {
        id: "disk4s1",
        name: "External",
        role: VolumeRole::User,
        mount_point: "/Volumes/External",
        device: EXTERNAL_DEVICE,
        mounted: true,
        purpose: "An external disk you attached.",
    },
];

/// The volumes of the fixture, in enumeration order.
///
/// Panics only if a constant in this file stops being a valid volume identifier.
pub fn mac_volumes() -> Vec<Volume> {
    FIXTURES.iter().map(volume).collect()
}

/// The mount table of the fixture: the mounted volumes only, firmlinks included.
///
/// Preboot and Recovery are in [`mac_volumes`] but not here. A mount table maps a
/// path to a volume, and a volume macOS keeps unmounted owns no path; inventing one
/// for it would make every lookup below `/System/Volumes/Preboot` answer with a
/// volume that is not actually there.
pub fn mac_mount_table() -> MountTable {
    let firmlinks: Vec<PathBuf> = DATA_FIRMLINKS.iter().map(PathBuf::from).collect();
    let entries = FIXTURES
        .iter()
        .filter(|fixture| fixture.mounted)
        .map(|fixture| MountEntry {
            mount_point: PathBuf::from(fixture.mount_point),
            device: fixture.device,
            volume: volume(fixture),
            firmlinks: if fixture.role == VolumeRole::Data { firmlinks.clone() } else { Vec::new() },
        })
        .collect();
    MountTable::new(entries)
}

/// Build the [`Volume`] of one fixture entry.
fn volume(fixture: &Fixture) -> Volume {
    Volume {
        id: volume_id(fixture.id),
        name: fixture.name.to_owned(),
        role: fixture.role,
        mount_point: fixture.mounted.then(|| PathBuf::from(fixture.mount_point)),
        used_bytes: 0,
        writable_by_broza: fixture.role.writable_by_broza(),
        purpose: fixture.purpose.to_owned(),
    }
}

/// Parse a volume identifier that this module controls.
fn volume_id(id: &str) -> VolumeId {
    id.parse().unwrap_or_else(|error| panic!("fixture volume id `{id}` is invalid: {error}"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{DATA_DEVICE, EXTERNAL_DEVICE, PREBOOT_DEVICE, SYSTEM_DEVICE, mac_mount_table, mac_volumes};
    use crate::model::VolumeRole;

    #[test]
    fn the_fixture_covers_every_role_broza_protects() {
        let roles: Vec<VolumeRole> = mac_volumes().iter().map(|volume| volume.role).collect();

        for role in [VolumeRole::System, VolumeRole::Data, VolumeRole::Vm, VolumeRole::Preboot] {
            assert!(roles.contains(&role), "{role:?} missing");
        }
        assert!(roles.contains(&VolumeRole::Recovery));
        assert!(roles.contains(&VolumeRole::User));
    }

    #[test]
    fn hidden_volumes_report_no_mount_point() {
        let unmounted: Vec<String> = mac_volumes()
            .iter()
            .filter(|volume| volume.mount_point.is_none())
            .map(|volume| volume.name.clone())
            .collect();

        assert_eq!(unmounted, vec!["Preboot".to_owned(), "Recovery".to_owned()]);
    }

    #[test]
    fn a_home_directory_resolves_to_the_data_volume_through_a_firmlink() {
        let table = mac_mount_table();

        assert_eq!(table.role_for(Path::new("/Users/dana/Library/Caches")), Some(VolumeRole::Data));
        assert_eq!(table.role_for(Path::new("/private/var/folders")), Some(VolumeRole::Data));
        assert_eq!(table.volume_for(Path::new("/Users/dana")).map(|e| e.device), Some(DATA_DEVICE));
    }

    #[test]
    fn system_paths_stay_on_the_read_only_system_volume() {
        let table = mac_mount_table();

        assert_eq!(table.role_for(Path::new("/usr/bin/diskutil")), Some(VolumeRole::System));
        assert_eq!(table.by_device(SYSTEM_DEVICE).map(|e| e.volume.role), Some(VolumeRole::System));
        assert_eq!(
            table.volume_for(Path::new("/System/Volumes/VM/sleepimage")).map(|e| e.volume.role),
            Some(VolumeRole::Vm)
        );
    }

    #[test]
    fn an_external_disk_is_the_only_other_writable_volume() {
        let table = mac_mount_table();

        let entry = table.volume_for(Path::new("/Volumes/External/Movies"));
        assert_eq!(entry.map(|e| e.device), Some(EXTERNAL_DEVICE));
        assert_eq!(entry.map(|e| e.volume.writable_by_broza), Some(true));
    }

    #[test]
    fn the_table_holds_the_mounted_volumes_and_nothing_else() {
        let mounted = mac_volumes().iter().filter(|volume| volume.mount_point.is_some()).count();
        let table = mac_mount_table();

        assert_eq!(table.entries().len(), mounted);
        assert_eq!(mounted, mac_volumes().len() - 2, "Preboot and Recovery stay out");
        assert!(table.by_device(PREBOOT_DEVICE).is_none());
        // Their paths still land on the sealed system volume, which is read-only
        // too, so leaving them out never turns a protected path into a writable one.
        assert_eq!(table.role_for(Path::new("/System/Volumes/Preboot/x")), Some(VolumeRole::System));
    }

    #[test]
    fn every_entry_has_its_own_device() {
        let table = mac_mount_table();
        let mut devices: Vec<u64> = table.entries().iter().map(|entry| entry.device).collect();
        devices.sort_unstable();
        devices.dedup();

        assert_eq!(devices.len(), table.entries().len());
    }
}
