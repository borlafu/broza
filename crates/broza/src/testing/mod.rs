//! Test doubles for every port in [`crate::ports`].
//!
//! Compiled for this crate's own unit tests and, for downstream crates, behind the
//! `test-support` feature. No test may touch the real `$HOME`, real disks, or spawn
//! `diskutil` (`AGENTS.md` §7); these fakes are how that rule is kept.
//!
//! [`fake_ports`] wires all of them into a [`Ports`] bundle and hands back the
//! [`Handles`] needed to configure and inspect each one. Every fake is configurable
//! through a shared reference, so the handles keep working after the bundle is moved
//! into the code under test.

pub mod fake_disks;
pub mod fake_fs;
mod fake_posix;
pub mod fake_prompter;
pub mod fake_runner;
mod fake_tree;
pub mod fixed_clock;
pub mod mac_fixture;
mod sync;

use std::sync::Arc;

pub use fake_disks::{FakeDisks, FakeSnapshots, FakeSpace};
pub use fake_fs::FakeFileOps;
pub use fake_prompter::{FakePrompter, RecordedPrompt};
pub use fake_runner::{FakeRunner, RecordedCall};
pub use fixed_clock::FixedClock;
pub use mac_fixture::{mac_mount_table, mac_volumes};

use crate::ports::{
    Answer, Clock, DiskEnumerator, FileOps, Ports, ProcessRunner, Prompter, SnapshotProvider, SpaceProvider,
};

/// Handles to the fakes inside a [`Ports`] bundle built by [`fake_ports`].
#[derive(Debug, Clone)]
pub struct Handles {
    /// The scripted process runner.
    pub process: Arc<FakeRunner>,
    /// The configured disk enumerator.
    pub disks: Arc<FakeDisks>,
    /// The configured purgeable-space provider.
    pub space: Arc<FakeSpace>,
    /// The configured snapshot provider.
    pub snapshots: Arc<FakeSnapshots>,
    /// The in-memory filesystem.
    pub fs: Arc<FakeFileOps>,
    /// The frozen clock.
    pub clock: Arc<FixedClock>,
    /// The scripted prompter.
    pub prompter: Arc<FakePrompter>,
}

/// A [`Ports`] bundle of fakes, plus the handles to drive and inspect them.
///
/// Everything starts empty and the prompter declines, so a test only configures what
/// it actually exercises and can never be authorised by accident.
pub fn fake_ports() -> (Ports, Handles) {
    let handles = Handles {
        process: Arc::new(FakeRunner::new()),
        disks: Arc::new(FakeDisks::empty()),
        space: Arc::new(FakeSpace::new()),
        snapshots: Arc::new(FakeSnapshots::new()),
        fs: Arc::new(FakeFileOps::new()),
        clock: Arc::new(FixedClock::default()),
        prompter: Arc::new(FakePrompter::scripted(&[Answer::No])),
    };
    let ports = Ports {
        process: Arc::clone(&handles.process) as Arc<dyn ProcessRunner>,
        disks: Arc::clone(&handles.disks) as Arc<dyn DiskEnumerator>,
        space: Arc::clone(&handles.space) as Arc<dyn SpaceProvider>,
        snapshots: Arc::clone(&handles.snapshots) as Arc<dyn SnapshotProvider>,
        fs: Arc::clone(&handles.fs) as Arc<dyn FileOps>,
        clock: Arc::clone(&handles.clock) as Arc<dyn Clock>,
        prompter: Arc::clone(&handles.prompter) as Arc<dyn Prompter>,
    };
    (ports, handles)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::fake_ports;
    use crate::ports::{Answer, ProcessOutput};

    #[test]
    fn the_bundle_and_the_handles_share_every_fake() {
        let (ports, handles) = fake_ports();
        let output = ProcessOutput { success: true, code: Some(0), stdout: b"x".to_vec(), stderr: vec![] };

        handles.process.script_output("diskutil", &["list"], output);
        handles.fs.add_root("/", 1);
        handles.prompter.queue(&[Answer::Yes]);

        let run = ports.process.run("diskutil", &["list"], Duration::from_secs(1));
        assert_eq!(run.map(|out| out.stdout_text()).unwrap_or_default(), "x");
        assert!(ports.fs.exists(Path::new("/")));
        assert_eq!(handles.process.calls().len(), 1);
    }

    #[test]
    fn a_fresh_bundle_declines_every_prompt() {
        let (ports, handles) = fake_ports();
        let request = crate::ports::ConfirmationRequest {
            max_risk: crate::model::Risk::Green,
            item_count: 1,
            total_bytes: 1,
            irreversible: false,
            preview: Vec::new(),
        };

        assert_eq!(ports.prompter.confirm(&request), Answer::No);
        assert_eq!(handles.prompter.requests().len(), 1);
    }

    #[test]
    fn a_fresh_bundle_knows_no_disks_and_no_purgeable_space() {
        let (ports, _handles) = fake_ports();

        assert!(ports.disks.enumerate().unwrap_or_else(|e| panic!("{e}")).disks.is_empty());
        assert_eq!(ports.space.purgeable_bytes(Path::new("/")).unwrap_or_else(|e| panic!("{e}")), 0);
    }
}
