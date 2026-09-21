//! Adapters: the only place where Broza talks to macOS.
//!
//! This is the sole module allowed to spawn processes, call `libc`/`objc2`, or use
//! `unsafe` (`AGENTS.md` §4). Everything here implements a trait from
//! [`crate::ports`], so the rest of the core stays pure and testable.

pub(crate) mod bulk_dir;
pub mod diskutil;
pub(crate) mod io_error;
pub mod mount_table;
pub mod nsurl_space;
pub mod process_error;
pub mod std_fs;
pub mod std_process;
pub mod system_clock;
pub mod tmutil_destinations;

use std::sync::Arc;

pub use diskutil::{DiskutilEnumerator, DiskutilSnapshots};
pub use mount_table::{FIRMLINKS_PATH, MountTableReport, parse_firmlinks, system_mount_table};
pub use nsurl_space::NsUrlSpaceProvider;
pub use process_error::{ProcessError, ProcessErrorKind};
pub use std_fs::StdFileOps;
pub use std_process::StdProcessRunner;
pub use system_clock::SystemClock;
pub use tmutil_destinations::{BackupDestinations, TMUTIL, parse_destination_info};

use crate::ports::{
    DiskEnumerator, FileOps, Ports, ProcessRunner, Prompter, SnapshotProvider, SpaceProvider,
};

/// The three storage ports of a real Mac, sharing one process runner.
///
/// They belong together: the enumerator needs the space provider to fill the
/// purgeable estimate of each container, and the enumerator and the snapshot
/// provider run their `diskutil` commands through the same runner, so a test
/// that scripts one scripts both.
pub fn system_disk_ports(
    runner: Arc<dyn ProcessRunner>,
) -> (Arc<dyn DiskEnumerator>, Arc<dyn SpaceProvider>, Arc<dyn SnapshotProvider>) {
    let space: Arc<dyn SpaceProvider> = Arc::new(NsUrlSpaceProvider);
    let fs: Arc<dyn FileOps> = Arc::new(StdFileOps);
    let disks: Arc<dyn DiskEnumerator> =
        Arc::new(DiskutilEnumerator::new(Arc::clone(&runner), Arc::clone(&space), fs));
    let snapshots: Arc<dyn SnapshotProvider> = Arc::new(DiskutilSnapshots::new(runner));
    (disks, space, snapshots)
}

/// Wire the real adapters into a [`Ports`] bundle.
///
/// The disk, space, and snapshot providers arrive from the caller — usually
/// straight from [`system_disk_ports`], but a test or a future GUI may pass its
/// own — and the prompter because it needs a terminal and therefore lives in
/// `broza-cli` (`AGENTS.md` §4).
pub fn system_ports(
    disks: Arc<dyn DiskEnumerator>,
    space: Arc<dyn SpaceProvider>,
    snapshots: Arc<dyn SnapshotProvider>,
    prompter: Arc<dyn Prompter>,
) -> Ports {
    Ports {
        process: Arc::new(StdProcessRunner),
        disks,
        space,
        snapshots,
        fs: Arc::new(StdFileOps),
        clock: Arc::new(SystemClock),
        prompter,
    }
}

/// Every real adapter of a Mac, wired into one [`Ports`] bundle.
///
/// The prompter is still the caller's: it needs a terminal.
pub fn system_ports_with_disks(prompter: Arc<dyn Prompter>) -> Ports {
    let (disks, space, snapshots) = system_disk_ports(Arc::new(StdProcessRunner));
    system_ports(disks, space, snapshots, prompter)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use super::diskutil::DISKUTIL;
    use super::tmutil_destinations::{DESTINATION_INFO_ARGS, TMUTIL};
    use super::{system_disk_ports, system_ports, system_ports_with_disks};
    use crate::model::VolumeId;
    use crate::ports::{Answer, ProcessOutput, ProcessRunner};
    use crate::testing::{FakeDisks, FakePrompter, FakeRunner, FakeSnapshots, FakeSpace};

    #[test]
    fn the_bundle_runs_real_commands_and_reads_the_real_filesystem() {
        let ports = system_ports(
            Arc::new(FakeDisks::empty()),
            Arc::new(FakeSpace::new()),
            Arc::new(FakeSnapshots::new()),
            Arc::new(FakePrompter::always(Answer::No)),
        );

        let out = ports
            .process
            .run("/bin/echo", &["wired"], Duration::from_secs(10))
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(out.stdout_text(), "wired\n");
        assert!(ports.fs.exists(Path::new("/usr/bin")));
        assert!(ports.clock.now() > jiff::Timestamp::UNIX_EPOCH);
    }

    /// The smallest machine `diskutil` can describe: one disk, no containers.
    const EMPTY_LIST: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>AllDisksAndPartitions</key><array/></dict></plist>"#;
    /// An `apfs list` output without a single container.
    const NO_CONTAINERS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Containers</key><array/></dict></plist>"#;
    /// A volume without snapshots.
    const NO_SNAPSHOTS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Snapshots</key><array/></dict></plist>"#;
    /// A Mac with Time Machine switched off.
    const NO_DESTINATIONS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict></dict></plist>"#;

    fn ok(stdout: &[u8]) -> ProcessOutput {
        ProcessOutput { success: true, code: Some(0), stdout: stdout.to_vec(), stderr: Vec::new() }
    }

    #[test]
    fn the_disk_ports_run_every_command_through_the_one_runner_they_were_given() {
        let runner = Arc::new(
            FakeRunner::new()
                .with_output(DISKUTIL, &["list", "-plist"], ok(EMPTY_LIST))
                .with_output(DISKUTIL, &["apfs", "list", "-plist"], ok(NO_CONTAINERS))
                .with_output(DISKUTIL, &["apfs", "listSnapshots", "-plist", "disk3s5"], ok(NO_SNAPSHOTS))
                .with_output(TMUTIL, &DESTINATION_INFO_ARGS, ok(NO_DESTINATIONS)),
        );
        let volume: VolumeId = "disk3s5".parse().unwrap_or_else(|e| panic!("{e}"));

        let (disks, _space, snapshots) = system_disk_ports(Arc::clone(&runner) as Arc<dyn ProcessRunner>);

        assert!(disks.enumerate().unwrap_or_else(|e| panic!("{e}")).disks.is_empty());
        assert!(snapshots.list(&volume).unwrap_or_else(|e| panic!("{e}")).is_empty());
        assert_eq!(runner.calls().len(), 4, "both adapters recorded on the same runner");
    }

    #[test]
    fn the_full_bundle_needs_nothing_but_a_prompter() {
        let ports = system_ports_with_disks(Arc::new(FakePrompter::always(Answer::No)));

        assert!(ports.fs.exists(Path::new("/usr/bin")));
        assert_eq!(format!("{ports:?}"), "Ports { .. }");
    }
}
