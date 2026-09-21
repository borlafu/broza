//! Adapters: the only place where Broza talks to macOS.
//!
//! This is the sole module allowed to spawn processes, call `libc`/`objc2`, or use
//! `unsafe` (`AGENTS.md` §4). Everything here implements a trait from
//! [`crate::ports`], so the rest of the core stays pure and testable.

pub mod diskutil;
pub(crate) mod io_error;
pub mod mount_table;
pub mod process_error;
pub mod std_fs;
pub mod std_process;
pub mod system_clock;

use std::sync::Arc;

pub use mount_table::{FIRMLINKS_PATH, MountTableReport, parse_firmlinks, system_mount_table};
pub use process_error::{ProcessError, ProcessErrorKind};
pub use std_fs::StdFileOps;
pub use std_process::StdProcessRunner;
pub use system_clock::SystemClock;

use crate::ports::{DiskEnumerator, Ports, Prompter, SnapshotProvider, SpaceProvider};

/// Wire the real adapters into a [`Ports`] bundle.
///
/// The disk, space, and snapshot providers arrive from the caller because they are
/// still milestone M2 work, and the prompter because it needs a terminal and
/// therefore lives in `broza-cli` (`AGENTS.md` §4).
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

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use super::system_ports;
    use crate::ports::Answer;
    use crate::testing::{FakeDisks, FakePrompter, FakeSnapshots, FakeSpace};

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
}
