//! The recorded machine `BROZA_FAKE_DISKUTIL_FIXTURES` substitutes for a Mac.
//!
//! Compiled only in debug builds with the `fake-diskutil` feature, so nothing
//! here can be reached by a released binary.
//!
//! Replacing the process runner alone is not enough to make a run
//! reproducible. Two of Broza's answers do not come from a command:
//!
//! - **purgeable space** comes from Foundation, which would report *this*
//!   Mac's figures whatever the recording says;
//! - **the mount table** comes from `stat` on each mount point and from
//!   `/usr/share/firmlinks`, which would describe *this* Mac's layout.
//!
//! So the space provider is pinned and the filesystem is an in-memory one that
//! knows exactly the mount points of the recording. What is left is a machine
//! a test can assert on byte for byte.

use std::path::Path;
use std::sync::Arc;

use broza::BrozaError;
use broza::adapters::{DiskutilEnumerator, DiskutilSnapshots, FIRMLINKS_PATH};
use broza::ports::{FileOps, ProcessRunner, SpaceProvider};
use broza::testing::{FakeFileOps, FakeSpace, fixture_runner};

use super::Machine;

/// Mount point the recorded machine reports purgeable space for.
const PURGEABLE_MOUNT: &str = "/System/Volumes/Data";
/// Purgeable bytes the recorded machine reports, 2.4 GB.
const PURGEABLE_BYTES: u64 = 2_400_000_000;

/// Every mount point of the recording, in the order `diskutil` lists them.
///
/// Each gets its own device id, counted from [`FIRST_DEVICE`], so the mount
/// table can tell them apart exactly as `st_dev` would on the real machine.
const MOUNT_POINTS: [&str; 14] = [
    "/",
    "/System/Volumes/Data",
    "/System/Volumes/Preboot",
    "/System/Volumes/Update",
    "/System/Volumes/VM",
    "/System/Volumes/iSCPreboot",
    "/System/Volumes/xarts",
    "/System/Volumes/Hardware",
    "/System/Volumes/Update/SFR/mnt1",
    "/Volumes/Kiro CLI",
    "/Volumes/Kiro CLI 1",
    "/Volumes/Kiro CLI 2",
    "/Volumes/Kiro CLI 3",
    "/private/var/folders/dd/bb00000000000000000000000000gn/T/com.docker.install/DockerDesktop-238018",
];
/// Device id of the first mount point; the rest follow in order.
const FIRST_DEVICE: u64 = 1;
/// `/usr/share/firmlinks` as macOS writes it, trimmed to what matters here.
const FIRMLINKS: &[u8] = b"/Applications\tApplications\n\
/Library\tLibrary\n\
/Users\tUsers\n\
/Volumes\tVolumes\n\
/opt\topt\n\
/private\tprivate\n\
/usr/local\tusr/local\n";
/// A handful of sized files on the Data volume, so a replayed `scan` has
/// consumers to list. Sizes are round on purpose: they are a fixture, not a
/// recording, and the snapshot should read as one.
const RECORDED_FILES: [(&str, u64); 7] = [
    ("/System/Volumes/Data/Users/dana/Library/Developer/Xcode/DerivedData/App/Build/app.o", 212_400_000_000),
    ("/System/Volumes/Data/Users/dana/Library/Caches/com.example.app/cache.db", 84_100_000_000),
    ("/System/Volumes/Data/Users/dana/Documents/thesis.pdf", 61_700_000_000),
    ("/System/Volumes/Data/Users/dana/code/old-site/node_modules/left-pad/index.js", 27_200_000_000),
    ("/System/Volumes/Data/Users/dana/Library/Logs/App/app.log", 1_200_000_000),
    ("/System/Volumes/Data/Users/dana/.Trash/old-disk-image.dmg", 3_300_000_000),
    (
        "/System/Volumes/Data/Users/dana/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
        38_600_000_000,
    ),
];
/// The home directory of the recorded machine, in the spelling its files use.
///
/// The seam swaps the filesystem for the recording, so the real `$HOME` of the
/// test process does not exist there; commands that walk the home walk this one.
pub const FIXTURE_HOME: &str = "/System/Volumes/Data/Users/dana";
/// Directories a test may ask `explain` about; the firmlinks send them to Data.
const FIRMLINKED_DIRS: [&str; 5] = ["/Applications", "/Library", "/Users", "/opt", "/private"];

/// The recorded machine in `dir`, wired as a [`Machine`].
///
/// # Errors
///
/// [`BrozaError::Io`] when the directory or one of its recordings cannot be read.
pub fn machine(dir: &Path) -> Result<Machine, BrozaError> {
    let process: Arc<dyn ProcessRunner> = Arc::new(fixture_runner(dir)?);
    let space: Arc<dyn SpaceProvider> =
        Arc::new(FakeSpace::new().with_purgeable(PURGEABLE_MOUNT, PURGEABLE_BYTES));
    let fs: Arc<dyn FileOps> = Arc::new(filesystem());
    let disks = Arc::new(DiskutilEnumerator::new(Arc::clone(&process), Arc::clone(&space), Arc::clone(&fs)));
    let snapshots = Arc::new(DiskutilSnapshots::new(Arc::clone(&process)));
    Ok(Machine { process, disks, space, snapshots, fs })
}

/// An in-memory filesystem shaped like the recorded machine.
fn filesystem() -> FakeFileOps {
    let fs = FakeFileOps::new();
    for (index, mount_point) in MOUNT_POINTS.iter().enumerate() {
        fs.add_root(mount_point, FIRST_DEVICE + index as u64);
    }
    fs.add_file(FIRMLINKS_PATH, FIRMLINKS);
    for directory in FIRMLINKED_DIRS {
        fs.add_dir(directory);
    }
    for (path, size_bytes) in RECORDED_FILES {
        fs.add_file(path, &[]);
        fs.set_size(path, size_bytes);
    }
    fs
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn every_mount_point_of_the_recording_has_a_device_of_its_own() {
        let fs = filesystem();

        let devices: Vec<u64> = MOUNT_POINTS
            .iter()
            .filter_map(|mount| fs.metadata(Path::new(mount)).ok())
            .map(|metadata| metadata.device)
            .collect();

        assert_eq!(devices.len(), MOUNT_POINTS.len(), "every mount point must be readable");
        let mut unique = devices.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), devices.len(), "{devices:?}");
    }

    #[test]
    fn the_firmlinked_directories_exist_so_explain_can_be_asked_about_them() {
        let fs = filesystem();

        for directory in FIRMLINKED_DIRS {
            assert!(fs.exists(Path::new(directory)), "{directory}");
        }
        assert!(!fs.exists(Path::new("/nowhere/at/all")), "the fake filesystem knows only what it was told");
    }

    #[test]
    fn the_firmlinks_file_is_readable_and_parses() {
        let fs = filesystem();

        let raw = fs.read(Path::new(FIRMLINKS_PATH)).unwrap_or_else(|e| panic!("{e}"));
        let parsed = broza::adapters::parse_firmlinks(&String::from_utf8_lossy(&raw));

        assert!(parsed.contains(&std::path::PathBuf::from("/Users")), "{parsed:?}");
        assert_eq!(parsed.len(), 7);
    }
}
