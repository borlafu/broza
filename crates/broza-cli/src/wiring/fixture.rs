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
use broza::detect::spotlight::{MDLS, MDLS_ARGS};
use broza::ports::{FileOps, ProcessOutput, ProcessRunner, SpaceProvider};
use broza::testing::{FakeFileOps, FakeRunner, FakeSpace, fixture_runner};
use jiff::Timestamp;

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
const RECORDED_FILES: [(&str, u64); 12] = [
    ("/System/Volumes/Data/Users/dana/Library/Developer/Xcode/DerivedData/App/Build/app.o", 212_400_000_000),
    ("/System/Volumes/Data/Users/dana/Library/Caches/com.example.app/cache.db", 84_100_000_000),
    ("/System/Volumes/Data/Users/dana/Documents/thesis.pdf", 61_700_000_000),
    ("/System/Volumes/Data/Users/dana/code/old-site/node_modules/left-pad/index.js", 27_200_000_000),
    ("/System/Volumes/Data/Users/dana/Library/Logs/App/app.log", 1_200_000_000),
    ("/System/Volumes/Data/Users/dana/.Trash/old-disk-image.dmg", 3_300_000_000),
    (OLD_MOVIE, 4_200_000_000),
    (INSTALLER, 800_000_000),
    (INSTALLER_COPY, 800_000_000),
    (
        "/System/Volumes/Data/Users/dana/Library/Mobile Documents/com~apple~CloudDocs/Photos/trip.heic",
        12_500_000_000,
    ),
    ("/System/Volumes/Data/Users/dana/Library/CloudStorage/Dropbox-Personal/work.pdf", 700_000_000),
    (
        "/System/Volumes/Data/Users/dana/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
        38_600_000_000,
    ),
];
/// A big file nobody has opened in years, for `large-old-files`.
const OLD_MOVIE: &str = "/System/Volumes/Data/Users/dana/Movies/holiday-2019.mov";
/// When [`OLD_MOVIE`] was last written and read, according to the filesystem.
const OLD_MOVIE_TOUCHED: &str = "2019-08-10T14:00:00Z";
/// What Spotlight answers for each big file under the home, as `mdls
/// -name kMDItemLastUsedDate -raw` prints it: the movie was last opened in
/// 2020, the thesis this summer, and the orphan module is unknown to it.
const SPOTLIGHT_ANSWERS: [(&str, &str); 8] = [
    (OLD_MOVIE, "2020-01-05 18:30:00 +0000"),
    (OLD_APP, "2024-01-10 09:00:00 +0000"),
    (FRESH_APP, "2026-09-01 09:00:00 +0000"),
    (MYSTERY_APP, "(null)"),
    (INSTALLER, "(null)"),
    (INSTALLER_COPY, "(null)"),
    ("/System/Volumes/Data/Users/dana/Documents/thesis.pdf", "2026-08-01 09:00:00 +0000"),
    ("/System/Volumes/Data/Users/dana/code/old-site/node_modules/left-pad/index.js", "(null)"),
];
/// An installer downloaded once and copied to the desktop: a duplicate pair.
/// The recording has no file contents, so equal sizes hash equal, as two real
/// copies would.
const INSTALLER: &str = "/System/Volumes/Data/Users/dana/Downloads/Xcode-installer.dmg";
const INSTALLER_COPY: &str = "/System/Volumes/Data/Users/dana/Desktop/Xcode-installer copy.dmg";
/// When the download landed: before the copy's default time, so the download
/// is the one kept, and within the year, so neither is a large old file.
const INSTALLER_TOUCHED: &str = "2025-12-01T09:00:00Z";
/// An application nobody has opened since 2024, and one opened this month.
const OLD_APP: &str = "/Applications/OldEditor.app";
const FRESH_APP: &str = "/Applications/Fresh.app";
/// An application Spotlight has no date for: listed, never acted on.
const MYSTERY_APP: &str = "/Applications/Mystery.app";
/// A leftover of an application no longer installed, untouched since 2023.
const LEFTOVER: &str = "/System/Volumes/Data/Users/dana/Library/HTTPStorages/com.gone.Tool";
const LEFTOVER_TOUCHED: &str = "2023-03-01T09:00:00Z";
/// A folder Full Disk Access would be needed for; the recorded process has none.
pub const DENIED_DIR: &str = "/System/Volumes/Data/Users/dana/Library/Accounts";
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
    let runner = fixture_runner(dir)?;
    script_spotlight(&runner);
    let process: Arc<dyn ProcessRunner> = Arc::new(runner);
    let space: Arc<dyn SpaceProvider> =
        Arc::new(FakeSpace::new().with_purgeable(PURGEABLE_MOUNT, PURGEABLE_BYTES));
    let fs: Arc<dyn FileOps> = Arc::new(filesystem());
    let disks = Arc::new(DiskutilEnumerator::new(Arc::clone(&process), Arc::clone(&space), Arc::clone(&fs)));
    let snapshots = Arc::new(DiskutilSnapshots::new(Arc::clone(&process)));
    Ok(Machine { process, disks, space, snapshots, fs })
}

/// Answer `mdls` for every big file of the recording, so `suggest` never asks
/// this Mac's Spotlight about a file that exists only in the fixture.
fn script_spotlight(runner: &FakeRunner) {
    for (path, answer) in SPOTLIGHT_ANSWERS {
        let args: Vec<&str> = MDLS_ARGS.iter().copied().chain([path]).collect();
        let output = ProcessOutput {
            success: true,
            code: Some(0),
            stdout: format!("{answer}\n").into_bytes(),
            stderr: Vec::new(),
        };
        runner.script_output(MDLS, &args, output);
    }
}

/// The manifest of an application bundle carrying `id`.
fn info_plist(id: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{id}</string></dict></plist>"#
    )
}

/// An in-memory filesystem shaped like the recorded machine.
fn filesystem() -> FakeFileOps {
    let fs = FakeFileOps::new();
    for (index, mount_point) in MOUNT_POINTS.iter().enumerate() {
        fs.add_root(mount_point, FIRST_DEVICE + index as u64);
    }
    fs.add_file(FIRMLINKS_PATH, FIRMLINKS);
    // Firmlinked directories live on the Data volume, whatever their spelling.
    let data_device = FIRST_DEVICE + 1;
    for directory in FIRMLINKED_DIRS {
        fs.add_root(directory, data_device);
    }
    for (path, size_bytes) in RECORDED_FILES {
        fs.add_file(path, &[]);
        fs.set_size(path, size_bytes);
    }
    if let Ok(touched) = OLD_MOVIE_TOUCHED.parse::<Timestamp>() {
        fs.set_times(OLD_MOVIE, touched, touched);
    }
    if let Ok(touched) = INSTALLER_TOUCHED.parse::<Timestamp>() {
        fs.set_times(INSTALLER, touched, touched);
    }
    for (app, id, size) in [
        (OLD_APP, "com.old.Editor", 3_000_000_000_u64),
        (FRESH_APP, "com.fresh.App", 900_000_000),
        (MYSTERY_APP, "org.mystery.App", 500_000_000),
    ] {
        fs.add_file(format!("{app}/Contents/Info.plist"), info_plist(id).as_bytes());
        fs.add_file(format!("{app}/Contents/MacOS/bin"), &[]);
        fs.set_size(format!("{app}/Contents/MacOS/bin"), size);
    }
    if let Ok(touched) = OLD_MOVIE_TOUCHED.parse::<Timestamp>() {
        fs.set_times(MYSTERY_APP, touched, touched);
    }
    fs.add_file(format!("{LEFTOVER}/data.db"), &[]);
    fs.set_size(format!("{LEFTOVER}/data.db"), 1_100_000_000);
    if let Ok(touched) = LEFTOVER_TOUCHED.parse::<Timestamp>() {
        fs.set_times(LEFTOVER, touched, touched);
        fs.set_times(format!("{LEFTOVER}/data.db"), touched, touched);
    }
    // A folder macOS keeps from a process without Full Disk Access.
    fs.add_dir(DENIED_DIR);
    fs.add_denied(DENIED_DIR);
    // Evicted from this disk: the provider holds it, the walk counts it as nothing.
    fs.add_dataless_file(
        "/System/Volumes/Data/Users/dana/Library/Mobile Documents/com~apple~CloudDocs/archive.zip",
        40_000_000_000,
    );
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
