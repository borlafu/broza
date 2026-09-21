//! Where the real macOS adapters are wired into a [`Ports`] bundle.
//!
//! The core never chooses its own adapters (`AGENTS.md` §4): this module is the
//! single place in the binary that says "the disks come from `diskutil`, the
//! clock from the system, the answers from the terminal". Everything below the
//! CLI sees only the traits.
//!
//! One [`ProcessRunner`] is shared by every adapter *and* by the host provider,
//! so a run of Broza executes external commands through exactly one object —
//! which is what makes the debug-only fixture seam below able to replace the
//! whole machine at once.

use std::sync::Arc;

use broza::BrozaError;
use broza::adapters::{StdFileOps, StdProcessRunner, SystemClock, system_disk_ports};
use broza::ports::{Ports, ProcessRunner, Prompter};

use crate::env::RuntimeEnv;
use crate::tty_prompter::TtyPrompter;

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

/// [`ports`] with a prompter the caller chose; the seam tests use.
///
/// # Errors
///
/// See [`ports`].
pub fn ports_with_prompter(runtime: &RuntimeEnv, prompter: Arc<dyn Prompter>) -> Result<Ports, BrozaError> {
    let process = process_runner(runtime)?;
    let (disks, space, snapshots) = system_disk_ports(Arc::clone(&process));
    Ok(Ports {
        process,
        disks,
        space,
        snapshots,
        fs: Arc::new(StdFileOps),
        clock: Arc::new(SystemClock),
        prompter,
    })
}

/// The runner every external command of this run goes through.
///
/// # Errors
///
/// See [`ports`].
pub fn process_runner(runtime: &RuntimeEnv) -> Result<Arc<dyn ProcessRunner>, BrozaError> {
    match fixture_runner(runtime)? {
        Some(fake) => Ok(fake),
        None => Ok(Arc::new(StdProcessRunner)),
    }
}

/// The recorded machine `BROZA_FAKE_DISKUTIL_FIXTURES` asks for, if any.
///
/// # Errors
///
/// [`BrozaError::Io`] when the directory or one of its recordings cannot be read.
#[cfg(all(debug_assertions, feature = "fake-diskutil"))]
fn fixture_runner(runtime: &RuntimeEnv) -> Result<Option<Arc<dyn ProcessRunner>>, BrozaError> {
    let Some(dir) = runtime.fake_diskutil_fixtures.as_deref() else { return Ok(None) };
    let runner = broza::testing::fixture_runner(dir)?;
    Ok(Some(Arc::new(runner)))
}

/// Release builds have no seam at all: there is nothing to replace the system with.
#[cfg(not(all(debug_assertions, feature = "fake-diskutil")))]
#[allow(clippy::unnecessary_wraps)]
const fn fixture_runner(_runtime: &RuntimeEnv) -> Result<Option<Arc<dyn ProcessRunner>>, BrozaError> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::*;

    fn runtime() -> RuntimeEnv {
        RuntimeEnv::for_tests(PathBuf::from("/Users/test"))
    }

    #[cfg(all(debug_assertions, feature = "fake-diskutil"))]
    fn fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../broza/tests/fixtures/plist/macos26")
    }

    #[test]
    fn the_default_bundle_talks_to_the_real_system() {
        let ports = ports(&runtime()).unwrap_or_else(|e| panic!("{e}"));

        // `/bin/echo` is the cheapest proof that the runner is the real one,
        // and reading `/usr/bin` the cheapest proof that the filesystem is.
        let out = ports.process.run("/bin/echo", &["wired"], Duration::from_secs(10));
        assert_eq!(out.map(|o| o.stdout_text()).unwrap_or_default(), "wired\n");
        assert!(ports.fs.exists(Path::new("/usr/bin")));
        assert!(ports.clock.now() > jiff::Timestamp::UNIX_EPOCH);
    }

    #[test]
    fn a_bundle_without_a_terminal_never_confirms_anything() {
        let ports = ports(&runtime()).unwrap_or_else(|e| panic!("{e}"));
        let request = broza::ports::ConfirmationRequest {
            max_risk: broza::model::Risk::Green,
            item_count: 1,
            total_bytes: 1,
            irreversible: false,
            preview: Vec::new(),
        };

        assert_eq!(ports.prompter.confirm(&request), broza::ports::Answer::NoTty);
    }

    #[cfg(all(debug_assertions, feature = "fake-diskutil"))]
    #[test]
    fn the_fixture_seam_replaces_every_adapter_that_runs_a_command() {
        use broza::adapters::diskutil::DISKUTIL;

        let runtime = RuntimeEnv { fake_diskutil_fixtures: Some(fixture_dir()), ..runtime() };

        let ports = ports(&runtime).unwrap_or_else(|e| panic!("{e}"));

        let report = ports.disks.enumerate().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(report.disks.first().map(|disk| disk.id.to_string()), Some("disk0".to_owned()));
        // The same runner answers a direct call, which is what lets the host
        // provider share it.
        let direct = ports.process.run(DISKUTIL, &["list", "-plist"], Duration::from_secs(1));
        assert!(direct.is_ok_and(|out| out.success));
    }

    #[cfg(all(debug_assertions, feature = "fake-diskutil"))]
    #[test]
    fn a_fixture_directory_that_does_not_exist_is_an_error_and_not_a_silent_fallback() {
        let runtime =
            RuntimeEnv { fake_diskutil_fixtures: Some(PathBuf::from("/nowhere/at/all")), ..runtime() };

        assert!(ports(&runtime).is_err());
    }
}
