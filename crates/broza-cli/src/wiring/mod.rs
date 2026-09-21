//! Where the real macOS adapters are wired into a [`Ports`] bundle.
//!
//! The core never chooses its own adapters (`AGENTS.md` §4): this module is
//! the single place in the binary that says "the disks come from `diskutil`,
//! the clock from the system, the answers from the terminal". Everything below
//! the CLI sees only the traits.
//!
//! One [`ProcessRunner`] is shared by every adapter *and* by the host
//! provider, so a run of Broza executes external commands through exactly one
//! object — which is what lets the debug-only seam in the private `fixture`
//! module replace a whole machine at once.

#[cfg(all(debug_assertions, feature = "fake-diskutil"))]
mod fixture;

/// The home a folder walk uses under the fixture seam, or `None` without it.
///
/// The seam swaps the filesystem for a recording in which the real `$HOME` does
/// not exist; the recording's own home stands in for it.
#[cfg(all(debug_assertions, feature = "fake-diskutil"))]
pub fn fixture_home(runtime: &crate::env::RuntimeEnv) -> Option<std::path::PathBuf> {
    runtime.fake_diskutil_fixtures.as_ref().map(|_| std::path::PathBuf::from(fixture::FIXTURE_HOME))
}

/// Without the seam there is no fixture home: the real `$HOME` is walked.
#[cfg(not(all(debug_assertions, feature = "fake-diskutil")))]
pub const fn fixture_home(_: &crate::env::RuntimeEnv) -> Option<std::path::PathBuf> {
    None
}

use std::sync::Arc;

use broza::BrozaError;
use broza::adapters::{StdFileOps, StdProcessRunner, SystemClock, system_disk_ports};
use broza::ports::{
    DiskEnumerator, FileOps, Ports, ProcessRunner, Prompter, SnapshotProvider, SpaceProvider,
};

use crate::env::RuntimeEnv;
use crate::tty_prompter::TtyPrompter;

/// Everything that answers questions about storage, real or recorded.
///
/// Grouped because they have to agree with each other: the enumerator asks the
/// space provider for each container's purgeable estimate and the filesystem
/// for a Time Machine marker, and an enumeration assembled from three
/// different machines would describe none of them.
pub struct Machine {
    /// Runs external commands.
    pub process: Arc<dyn ProcessRunner>,
    /// Enumerates disks, containers and volumes.
    pub disks: Arc<dyn DiskEnumerator>,
    /// Reports purgeable space per mount point.
    pub space: Arc<dyn SpaceProvider>,
    /// Lists APFS local snapshots.
    pub snapshots: Arc<dyn SnapshotProvider>,
    /// Filesystem reads: mount points, firmlinks, `explain`'s existence check.
    pub fs: Arc<dyn FileOps>,
}

/// Every real adapter of this Mac, wired for `runtime`.
///
/// The prompter is interactive only when [`RuntimeEnv::is_interactive`] says
/// so, which is how a run without a terminal ends in exit `7` rather than in a
/// prompt nobody can answer.
///
/// # Errors
///
/// [`BrozaError`] only from the debug-only fixture seam, when
/// `BROZA_FAKE_DISKUTIL_FIXTURES` names a directory that cannot be read.
pub fn ports(runtime: &RuntimeEnv) -> Result<Ports, BrozaError> {
    let prompter: Arc<dyn Prompter> = Arc::new(TtyPrompter::new(runtime.is_interactive()));
    ports_with_prompter(runtime, prompter)
}

/// [`ports`] with a prompter the caller chose.
///
/// # Errors
///
/// See [`ports`].
pub fn ports_with_prompter(runtime: &RuntimeEnv, prompter: Arc<dyn Prompter>) -> Result<Ports, BrozaError> {
    let Machine { process, disks, space, snapshots, fs } = machine(runtime)?;
    Ok(Ports { process, disks, space, snapshots, fs, clock: Arc::new(SystemClock), prompter })
}

/// The machine this run looks at: the real one, or a recording of one.
///
/// # Errors
///
/// See [`ports`].
fn machine(runtime: &RuntimeEnv) -> Result<Machine, BrozaError> {
    if let Some(recorded) = recorded_machine(runtime)? {
        return Ok(recorded);
    }
    let process: Arc<dyn ProcessRunner> = Arc::new(StdProcessRunner);
    let (disks, space, snapshots) = system_disk_ports(Arc::clone(&process));
    Ok(Machine { process, disks, space, snapshots, fs: Arc::new(StdFileOps) })
}

/// The recording `BROZA_FAKE_DISKUTIL_FIXTURES` asks for, if any.
///
/// # Errors
///
/// [`BrozaError::Io`] when the directory or one of its recordings cannot be read.
#[cfg(all(debug_assertions, feature = "fake-diskutil"))]
fn recorded_machine(runtime: &RuntimeEnv) -> Result<Option<Machine>, BrozaError> {
    match runtime.fake_diskutil_fixtures.as_deref() {
        Some(dir) => fixture::machine(dir).map(Some),
        None => Ok(None),
    }
}

/// Release builds have no seam at all: there is nothing to replace the system with.
#[cfg(not(all(debug_assertions, feature = "fake-diskutil")))]
#[allow(clippy::unnecessary_wraps)]
const fn recorded_machine(_runtime: &RuntimeEnv) -> Result<Option<Machine>, BrozaError> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;

    use broza::model::Risk;
    use broza::ports::{Answer, ConfirmationRequest};

    use super::*;

    fn runtime() -> RuntimeEnv {
        RuntimeEnv::for_tests(PathBuf::from("/Users/test"))
    }

    fn request() -> ConfirmationRequest {
        ConfirmationRequest {
            max_risk: Risk::Green,
            item_count: 1,
            total_bytes: 1,
            irreversible: false,
            preview: Vec::new(),
        }
    }

    /// Nothing in this module's tests touches the real machine: building the
    /// bundle runs no command and reads no path, and the one behaviour worth
    /// asserting — that a run without a terminal cannot be confirmed — needs
    /// neither (`AGENTS.md` §7).
    #[test]
    fn a_bundle_without_a_terminal_never_confirms_anything() {
        let ports = ports(&runtime()).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(ports.prompter.confirm(&request()), Answer::NoTty);
        assert_eq!(ports.prompter.confirm_literal(&request(), "PURGE"), Answer::NoTty);
    }

    #[test]
    fn the_bundle_hides_its_adapters_behind_the_traits() {
        let ports = ports(&runtime()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(format!("{ports:?}"), "Ports { .. }");
    }

    #[test]
    fn a_caller_may_supply_its_own_prompter() {
        use broza::testing::FakePrompter;

        let prompter = Arc::new(FakePrompter::always(Answer::Yes));
        let ports = ports_with_prompter(&runtime(), Arc::clone(&prompter) as Arc<dyn Prompter>)
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(ports.prompter.confirm(&request()), Answer::Yes);
        assert_eq!(prompter.requests().len(), 1);
    }

    #[cfg(all(debug_assertions, feature = "fake-diskutil"))]
    fn fixture_dir() -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../broza/tests/fixtures/plist/macos26")
    }

    #[cfg(all(debug_assertions, feature = "fake-diskutil"))]
    #[test]
    fn the_seam_replaces_the_commands_the_space_provider_and_the_filesystem() {
        use std::path::Path;

        let runtime = RuntimeEnv { fake_diskutil_fixtures: Some(fixture_dir()), ..runtime() };

        let ports = ports(&runtime).unwrap_or_else(|e| panic!("{e}"));

        let report = ports.disks.enumerate().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(report.disks.first().map(|disk| disk.id.to_string()), Some("disk0".to_owned()));
        assert!(ports.fs.exists(Path::new("/System/Volumes/Data")), "the recorded layout, not this Mac's");
        assert!(!ports.fs.exists(Path::new("/nowhere/at/all")));
        let purgeable = ports.space.purgeable_bytes(Path::new("/System/Volumes/Data"));
        assert_eq!(purgeable.unwrap_or_default(), 2_400_000_000);
    }

    #[cfg(all(debug_assertions, feature = "fake-diskutil"))]
    #[test]
    fn a_fixture_directory_that_does_not_exist_is_an_error_and_not_a_silent_fallback() {
        let runtime =
            RuntimeEnv { fake_diskutil_fixtures: Some(PathBuf::from("/nowhere/at/all")), ..runtime() };

        assert!(ports(&runtime).is_err());
    }
}
