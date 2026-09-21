//! Builds the [`MountTable`] from enumerated disks and the real device ids.
//!
//! The volumes come from a [`DiskEnumerator`](crate::ports::DiskEnumerator); this
//! module only adds what `diskutil` does not report: the `st_dev` of every mount
//! point and, for the Data volume, the firmlinks that make `/Users` live on it
//! (`docs/adr/0002-diskutil-plist-over-diskarbitration.md`). It never runs
//! `diskutil` itself.
//!
//! Nothing here aborts a scan. A volume Broza cannot stat is left out of the table
//! and reported as a warning, because an incomplete table still protects the volumes
//! it does know about, while a hard failure would leave the user with nothing.

use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::adapters::std_fs::StdFileOps;
use crate::model::{Disk, Volume, VolumeRole, Warning};
use crate::ports::FileOps;
use crate::scan::{MountEntry, MountTable};

/// Where macOS records the firmlinks of the current system.
pub const FIRMLINKS_PATH: &str = "/usr/share/firmlinks";
/// The only mount point whose Data volume owns the firmlinks of the boot group.
const DATA_VOLUME_MOUNT_POINT: &str = "/System/Volumes/Data";
/// Character separating the system path from the data-relative path.
const FIRMLINK_SEPARATOR: char = '\t';
/// Character starting a comment line.
const COMMENT_PREFIX: char = '#';
/// Warning code for a mount point Broza is not allowed to look at.
const PERMISSION_DENIED_CODE: &str = "permission_denied";
/// Warning code for a firmlinks file that exists but cannot be read.
const FIRMLINKS_UNREADABLE_CODE: &str = "firmlinks_unreadable";

/// A mount table and everything that had to be left out of it.
#[derive(Debug, Clone, Default)]
pub struct MountTableReport {
    /// The volumes Broza could resolve.
    pub table: MountTable,
    /// One entry per volume or firmlink Broza could not read.
    pub warnings: Vec<Warning>,
}

/// Mount table of the running system for `disks`.
pub fn system_mount_table(disks: &[Disk]) -> Result<MountTableReport, BrozaError> {
    mount_table_from(&StdFileOps, disks, Path::new(FIRMLINKS_PATH))
}

/// Mount table of `disks`, reading device ids and firmlinks through `fs`.
///
/// A volume with no mount point, or whose mount point has disappeared since the
/// enumeration, contributes no entry: an unmounted volume owns no path. A mount
/// point Broza is not allowed to stat is skipped with a warning. Anything else is
/// returned to the caller.
pub fn mount_table_from(
    fs: &dyn FileOps,
    disks: &[Disk],
    firmlinks_path: &Path,
) -> Result<MountTableReport, BrozaError> {
    let (firmlinks, mut warnings) = read_firmlinks(fs, firmlinks_path);
    let mut entries = Vec::new();
    for volume in disks.iter().flat_map(|disk| &disk.containers).flat_map(|container| &container.volumes) {
        match entry_for(fs, volume, &firmlinks) {
            Ok(Some(entry)) => entries.push(entry),
            Ok(None) => {}
            Err(BrozaError::PermissionDenied { path }) => warnings.push(Warning {
                code: PERMISSION_DENIED_CODE.to_owned(),
                message: format!("skipped volume {}: its mount point cannot be read", volume.id),
                path: Some(path),
            }),
            Err(error) => return Err(error),
        }
    }
    Ok(MountTableReport { table: MountTable::new(entries), warnings })
}

/// Parse the firmlinks file, keeping the absolute system paths.
///
/// Format: one `<system path>\t<data-relative path>` per line. Comments, blank
/// lines, and anything that is not an absolute system path are ignored, because an
/// unparsable line must never turn into a wrong volume attribution.
pub fn parse_firmlinks(contents: &str) -> Vec<PathBuf> {
    contents
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty() && !line.starts_with(COMMENT_PREFIX))
        .filter_map(|line| line.split_once(FIRMLINK_SEPARATOR))
        .map(|(system_path, _)| system_path.trim())
        .filter(|system_path| Path::new(system_path).is_absolute())
        .map(PathBuf::from)
        .collect()
}

/// Read and parse the firmlinks file; an absent one simply means no firmlinks.
///
/// A file that exists but cannot be read is a warning, not a failure: without
/// firmlinks `/Users` resolves to the read-only System volume, which is the
/// restrictive answer, never the dangerous one.
fn read_firmlinks(fs: &dyn FileOps, path: &Path) -> (Vec<PathBuf>, Vec<Warning>) {
    match fs.read(path) {
        Ok(raw) => (parse_firmlinks(&String::from_utf8_lossy(&raw)), Vec::new()),
        Err(BrozaError::TargetNotFound(_)) => (Vec::new(), Vec::new()),
        Err(error) => (
            Vec::new(),
            vec![Warning {
                code: FIRMLINKS_UNREADABLE_CODE.to_owned(),
                message: format!(
                    "firmlinks could not be read, paths may be attributed to the system volume: {error}"
                ),
                path: Some(path.to_path_buf()),
            }],
        ),
    }
}

/// Build the entry of one volume, or `None` when it owns no live mount point.
fn entry_for(
    fs: &dyn FileOps,
    volume: &Volume,
    firmlinks: &[PathBuf],
) -> Result<Option<MountEntry>, BrozaError> {
    let Some(mount_point) = volume.mount_point.clone() else { return Ok(None) };
    let device = match fs.metadata(&mount_point) {
        Ok(metadata) => metadata.device,
        Err(BrozaError::TargetNotFound(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    let firmlinks = if owns_firmlinks(volume, &mount_point) { firmlinks.to_vec() } else { Vec::new() };
    Ok(Some(MountEntry { mount_point, device, volume: volume.clone(), firmlinks }))
}

/// `true` only for the Data volume of the boot group.
///
/// A second APFS container, or an external disk restored from a system image, can
/// also carry role `data`; giving it `/Users` would send every home directory to the
/// wrong volume.
fn owns_firmlinks(volume: &Volume, mount_point: &Path) -> bool {
    volume.role == VolumeRole::Data && mount_point == Path::new(DATA_VOLUME_MOUNT_POINT)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{FIRMLINKS_PATH, MountTableReport, mount_table_from, parse_firmlinks};
    use crate::model::{Container, Disk, FsKind, Volume, VolumeRole};
    use crate::testing::FakeFileOps;

    /// A firmlinks file as macOS writes it, plus the lines a parser must survive.
    const FIRMLINKS_SAMPLE: &str = "\
# generated by the installer

/Applications\tApplications
/Users\tUsers
/private\tprivate
relative/path\trelative
/no-tab-here
";

    fn volume(id: &str, role: VolumeRole, mount: Option<&str>) -> Volume {
        Volume {
            id: id.parse().unwrap_or_else(|e| panic!("{e}")),
            name: id.to_owned(),
            uuid: None,
            role,
            mount_point: mount.map(PathBuf::from),
            used_bytes: 0,
            writable_by_broza: role.writable_by_broza(),
            purpose: String::new(),
        }
    }

    fn disks(volumes: Vec<Volume>) -> Vec<Disk> {
        vec![Disk {
            id: "disk0".parse().unwrap_or_else(|e| panic!("{e}")),
            model: "Apple SSD".to_owned(),
            size_bytes: 0,
            internal: true,
            containers: vec![Container {
                id: "disk3".parse().unwrap_or_else(|e| panic!("{e}")),
                kind: FsKind::Apfs,
                size_bytes: 0,
                used_bytes: 0,
                free_bytes: 0,
                purgeable_bytes: 0,
                volumes,
            }],
        }]
    }

    fn mac_like_fs() -> FakeFileOps {
        FakeFileOps::new()
            .with_root("/", 1)
            .with_root("/System/Volumes/Data", 2)
            .with_root("/Volumes/Clone", 3)
            .with_file(FIRMLINKS_PATH, FIRMLINKS_SAMPLE.as_bytes())
    }

    fn boot_volumes() -> Vec<Volume> {
        vec![
            volume("disk3s1", VolumeRole::System, Some("/")),
            volume("disk3s5", VolumeRole::Data, Some("/System/Volumes/Data")),
        ]
    }

    fn report(fs: &FakeFileOps, volumes: Vec<Volume>) -> MountTableReport {
        mount_table_from(fs, &disks(volumes), Path::new(FIRMLINKS_PATH)).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn parsing_keeps_the_absolute_system_paths_only() {
        let parsed = parse_firmlinks(FIRMLINKS_SAMPLE);

        assert_eq!(
            parsed,
            vec![PathBuf::from("/Applications"), PathBuf::from("/Users"), PathBuf::from("/private")]
        );
    }

    #[test]
    fn parsing_an_empty_file_yields_no_firmlinks() {
        assert!(parse_firmlinks("").is_empty());
        assert!(parse_firmlinks("\n\n# only comments\n").is_empty());
    }

    #[test]
    fn every_mounted_volume_gets_the_device_of_its_mount_point() {
        let report = report(&mac_like_fs(), boot_volumes());

        assert_eq!(report.table.entries().len(), 2);
        assert_eq!(report.table.by_device(1).map(|entry| entry.volume.role), Some(VolumeRole::System));
        assert_eq!(report.table.by_device(2).map(|entry| entry.volume.role), Some(VolumeRole::Data));
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn only_the_data_volume_of_the_boot_group_carries_the_firmlinks() {
        let report = report(&mac_like_fs(), boot_volumes());

        let data = report.table.by_device(2).unwrap_or_else(|| panic!("no data volume"));
        assert!(data.firmlinks.contains(&PathBuf::from("/Users")));
        assert_eq!(report.table.by_device(1).map(|entry| entry.firmlinks.len()), Some(0));
    }

    #[test]
    fn a_second_data_volume_elsewhere_does_not_claim_the_firmlinks() {
        let mut volumes = boot_volumes();
        volumes.push(volume("disk5s1", VolumeRole::Data, Some("/Volumes/Clone")));

        let report = report(&mac_like_fs(), volumes);

        assert_eq!(report.table.by_device(3).map(|entry| entry.firmlinks.len()), Some(0));
        assert_eq!(report.table.by_device(2).map(|entry| entry.firmlinks.len()), Some(3));
    }

    #[test]
    fn a_firmlinked_path_resolves_to_the_data_volume() {
        let report = report(&mac_like_fs(), boot_volumes());

        assert_eq!(report.table.role_for(Path::new("/Users/dana/Downloads")), Some(VolumeRole::Data));
        assert_eq!(report.table.role_for(Path::new("/System/Library")), Some(VolumeRole::System));
    }

    #[test]
    fn a_volume_without_a_mount_point_produces_no_entry() {
        let report = report(&mac_like_fs(), vec![volume("disk3s2", VolumeRole::Preboot, None)]);

        assert!(report.table.entries().is_empty());
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn a_mount_point_that_disappeared_is_skipped() {
        let report = report(&mac_like_fs(), vec![volume("disk4s1", VolumeRole::User, Some("/Volumes/Gone"))]);

        assert!(report.table.entries().is_empty());
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn a_missing_firmlinks_file_leaves_the_data_volume_without_firmlinks() {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);

        let report = mount_table_from(&fs, &disks(boot_volumes()), Path::new("/nowhere/firmlinks"))
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(report.table.by_device(2).map(|entry| entry.firmlinks.len()), Some(0));
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn an_unreadable_firmlinks_file_is_a_warning_and_not_a_failure() {
        let fs = mac_like_fs();
        fs.add_dir("/usr/share/firmlinks-as-a-directory");

        let report =
            mount_table_from(&fs, &disks(boot_volumes()), Path::new("/usr/share/firmlinks-as-a-directory"))
                .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].code, "firmlinks_unreadable");
        assert_eq!(report.table.entries().len(), 2);
        assert_eq!(report.table.by_device(2).map(|entry| entry.firmlinks.len()), Some(0));
    }

    #[test]
    fn a_mount_point_broza_may_not_read_is_skipped_with_a_warning() {
        let fs = mac_like_fs();
        fs.add_denied("/System/Volumes/Data");

        let report = report(&fs, boot_volumes());

        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].code, "permission_denied");
        assert_eq!(report.warnings[0].path, Some(PathBuf::from("/System/Volumes/Data")));
        assert_eq!(report.table.entries().len(), 1, "the system volume is still protected");
        assert_eq!(report.table.role_for(Path::new("/usr/bin")), Some(VolumeRole::System));
    }

    #[test]
    fn entries_keep_the_order_the_enumerator_reported() {
        let mut volumes = boot_volumes();
        volumes.reverse();

        let report = report(&mac_like_fs(), volumes);

        let devices: Vec<u64> = report.table.entries().iter().map(|entry| entry.device).collect();
        assert_eq!(devices, vec![2, 1]);
    }
}
