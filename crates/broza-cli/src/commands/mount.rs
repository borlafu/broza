//! Building the mount table through the [`FileOps`](broza::ports::FileOps) port.
//!
//! `broza::adapters::system_mount_table` is the convenience wrapper that reads
//! the real filesystem directly. The CLI does not use it: every command here
//! goes through `ports.fs`, so a test can hand Broza a filesystem of its own
//! and the answer to "which volume is this path on" stops depending on the
//! machine the test happens to run on (`AGENTS.md` §7).

use broza::BrozaError;
use broza::adapters::mount_table::mount_table_from;
use broza::adapters::{FIRMLINKS_PATH, MountTableReport};
use broza::model::Disk;
use broza::ports::Ports;

use std::path::Path;

/// The mount table of `disks`, read through `ports.fs`.
///
/// # Errors
///
/// Whatever the mount table returns when a mount point cannot be read for a
/// reason other than "absent" or "not permitted"; both of those are warnings.
pub fn mount_table(ports: &Ports, disks: &[Disk]) -> Result<MountTableReport, BrozaError> {
    mount_table_from(ports.fs.as_ref(), disks, Path::new(FIRMLINKS_PATH))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;

    use broza::model::{Container, FsKind, Volume, VolumeRole};
    use broza::testing::fake_ports;

    use super::*;

    fn disks() -> Vec<Disk> {
        vec![Disk {
            id: "disk0".parse().unwrap_or_else(|e| panic!("{e}")),
            model: "APPLE SSD".to_owned(),
            size_bytes: 1,
            internal: true,
            containers: vec![Container {
                id: "disk3".parse().unwrap_or_else(|e| panic!("{e}")),
                kind: FsKind::Apfs,
                size_bytes: 1,
                used_bytes: 1,
                free_bytes: 0,
                purgeable_bytes: 0,
                volumes: vec![Volume {
                    id: "disk3s5".parse().unwrap_or_else(|e| panic!("{e}")),
                    name: "Data".to_owned(),
                    role: VolumeRole::Data,
                    mount_point: Some(PathBuf::from("/System/Volumes/Data")),
                    used_bytes: 1,
                    writable_by_broza: true,
                    purpose: String::new(),
                }],
            }],
        }]
    }

    #[test]
    fn the_table_is_read_through_the_port_and_not_from_the_real_disk() {
        let (ports, handles) = fake_ports();
        handles.fs.add_root("/System/Volumes/Data", 42);
        handles.fs.add_file(FIRMLINKS_PATH, b"/Users\tUsers\n");

        let report = mount_table(&ports, &disks()).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(report.table.by_device(42).map(|entry| entry.volume.role), Some(VolumeRole::Data));
        assert_eq!(
            report.table.role_for(Path::new("/Users/dana")),
            Some(VolumeRole::Data),
            "the firmlinks file came from the port too"
        );
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn a_filesystem_that_knows_nothing_yields_an_empty_table_and_no_failure() {
        let (ports, _handles) = fake_ports();

        let report = mount_table(&ports, &disks()).unwrap_or_else(|e| panic!("{e}"));

        assert!(report.table.entries().is_empty());
        assert!(report.warnings.is_empty());
    }
}
