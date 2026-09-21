//! A [`FakeRunner`] built from a directory of recorded `diskutil` / `tmutil` plists.
//!
//! The recordings are the ones `scripts/capture-diskutil-fixtures.sh` writes
//! into `crates/broza/tests/fixtures/plist/<macos major>/`. Naming them is the
//! whole mapping:
//!
//! | File | Command |
//! |---|---|
//! | `list.plist` | `diskutil list -plist` |
//! | `apfs_list.plist` | `diskutil apfs list -plist` |
//! | `tmutil_destinationinfo.plist` | `tmutil destinationinfo -X` |
//! | `info_*.plist` | `diskutil info -plist <DeviceIdentifier>` |
//!
//! An `info_*.plist` is filed under the identifier *inside* the recording
//! rather than the one in its file name, so `info_data.plist` answers
//! `diskutil info -plist disk3s5` without a table to keep in step.
//!
//! This exists so the `broza` binary can be driven end to end without a disk:
//! the CLI swaps its [`ProcessRunner`](crate::ports::ProcessRunner) for one of
//! these when `BROZA_FAKE_DISKUTIL_FIXTURES` is set in a debug build.

use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::adapters::diskutil::{DISKUTIL, parse_info};
use crate::adapters::tmutil_destinations::{DESTINATION_INFO_ARGS, TMUTIL};
use crate::ports::ProcessOutput;
use crate::testing::FakeRunner;

/// Recording of `diskutil list -plist`.
const LIST_FIXTURE: &str = "list.plist";
/// Recording of `diskutil apfs list -plist`.
const APFS_LIST_FIXTURE: &str = "apfs_list.plist";
/// Recording of `tmutil destinationinfo -X`.
const DESTINATION_INFO_FIXTURE: &str = "tmutil_destinationinfo.plist";
/// Prefix of every `diskutil info -plist <device>` recording.
const INFO_PREFIX: &str = "info_";
/// Suffix of every recording this module reads.
const PLIST_SUFFIX: &str = ".plist";

/// A runner answering every command an enumeration issues, out of `dir`.
///
/// `dir` is a directory of recordings such as
/// `crates/broza/tests/fixtures/plist/macos26`. The three whole-machine
/// recordings are required; the `info_*.plist` files are whatever the
/// directory happens to hold.
///
/// # Errors
///
/// [`BrozaError::Io`] when `dir` or a required recording cannot be read, and
/// [`BrozaError::Other`] when an `info_*.plist` is not a plist Broza can parse.
pub fn fixture_runner(dir: &Path) -> Result<FakeRunner, BrozaError> {
    let runner = FakeRunner::new();
    runner.script_output(DISKUTIL, &["list", "-plist"], recorded(&dir.join(LIST_FIXTURE))?);
    runner.script_output(DISKUTIL, &["apfs", "list", "-plist"], recorded(&dir.join(APFS_LIST_FIXTURE))?);
    runner.script_output(TMUTIL, &DESTINATION_INFO_ARGS, recorded(&dir.join(DESTINATION_INFO_FIXTURE))?);
    for path in info_recordings(dir)? {
        script_info(&runner, &path)?;
    }
    Ok(runner)
}

/// Every `info_*.plist` in `dir`, in a stable order.
fn info_recordings(dir: &Path) -> Result<Vec<PathBuf>, BrozaError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|source| BrozaError::from_io("read fixture directory", dir, source))?;
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_info_recording(path))
        .collect();
    paths.sort();
    Ok(paths)
}

/// `true` for a file named `info_<something>.plist`.
fn is_info_recording(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(INFO_PREFIX) && name.ends_with(PLIST_SUFFIX))
}

/// File `path` under the device identifier the recording itself declares.
fn script_info(runner: &FakeRunner, path: &Path) -> Result<(), BrozaError> {
    let output = recorded(path)?;
    let info = parse_info(&output.stdout)?;
    if info.device_identifier.is_empty() {
        return Err(BrozaError::Other(format!("fixture {} declares no DeviceIdentifier", path.display())));
    }
    runner.script_output(DISKUTIL, &["info", "-plist", &info.device_identifier], output);
    Ok(())
}

/// The contents of `path` as the standard output of a successful command.
fn recorded(path: &Path) -> Result<ProcessOutput, BrozaError> {
    let stdout = std::fs::read(path).map_err(|source| BrozaError::from_io("read fixture", path, source))?;
    Ok(ProcessOutput { success: true, code: Some(0), stdout, stderr: Vec::new() })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::fixture_runner;
    use crate::adapters::diskutil::DISKUTIL;
    use crate::adapters::tmutil_destinations::{DESTINATION_INFO_ARGS, TMUTIL};
    use crate::ports::ProcessRunner;

    const TIMEOUT: Duration = Duration::from_secs(1);

    fn macos26() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plist/macos26")
    }

    #[test]
    fn the_recorded_machine_answers_the_three_whole_machine_commands() {
        let runner = fixture_runner(&macos26()).unwrap_or_else(|e| panic!("{e}"));

        for (program, args) in [
            (DISKUTIL, vec!["list", "-plist"]),
            (DISKUTIL, vec!["apfs", "list", "-plist"]),
            (TMUTIL, DESTINATION_INFO_ARGS.to_vec()),
        ] {
            let out = runner.run(program, &args, TIMEOUT).unwrap_or_else(|e| panic!("{program}: {e}"));
            assert!(out.success);
            assert!(out.stdout_text().contains("<plist"), "{program} {args:?} returned no plist");
        }
    }

    #[test]
    fn an_info_recording_is_filed_under_the_identifier_it_declares() {
        let runner = fixture_runner(&macos26()).unwrap_or_else(|e| panic!("{e}"));

        // `info_data.plist` is the recording of the data volume, whose BSD name
        // is `disk3s5`; nothing in the file name says so.
        let out = runner.run(DISKUTIL, &["info", "-plist", "disk3s5"], TIMEOUT);

        assert!(out.is_ok_and(|o| o.stdout_text().contains("disk3s5")));
    }

    #[test]
    fn every_physical_disk_of_the_recording_can_be_asked_about() {
        let runner = fixture_runner(&macos26()).unwrap_or_else(|e| panic!("{e}"));

        for device in ["disk0", "disk4", "disk5", "disk6", "disk7", "disk8"] {
            assert!(runner.run(DISKUTIL, &["info", "-plist", device], TIMEOUT).is_ok(), "{device}");
        }
    }

    #[test]
    fn a_directory_without_the_recordings_is_an_error_and_not_an_empty_machine() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));

        let error = fixture_runner(dir.path()).err();

        assert!(error.is_some(), "a missing list.plist must fail loudly");
    }
}
