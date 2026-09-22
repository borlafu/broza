//! `ios-simulators`: simulator devices nobody boots (`docs/cli-spec.md` §3.3).
//!
//! One finding, `ios-simulators.devices`, amber and `quarantine`. The device
//! list comes from `xcrun simctl list -j devices` through the process port,
//! with a short timeout: `xcrun` can hang when Xcode is half installed, and a
//! machine without Xcode has no simulators to speak of. A device is proposed
//! when its runtime is gone (`isAvailable: false`) or when it was last booted
//! more than `--unused-after` ago; a device that never booted and is still
//! available is left alone, because it was just created. The path is the
//! device's directory under `~/Library/Developer/CoreSimulator/Devices`, sized
//! from the walk (falling back to `dataPathSize`). A failing or missing `xcrun`
//! is a `location_unreadable` warning, never a failure.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use jiff::Timestamp;
use serde::Deserialize;

use crate::BrozaError;
use crate::model::{Category, FindingPath};

use super::support::{by_size_then_path, finish, path_with, start};
use crate::detect::detector::{DetectContext, Detected, Detector};

/// Xcode's command runner.
pub const XCRUN: &str = "/usr/bin/xcrun";
/// The listing Broza asks for: devices only, as JSON.
pub const SIMCTL_LIST_ARGS: [&str; 4] = ["simctl", "list", "-j", "devices"];
/// `simctl` answers in well under a second when Xcode is healthy.
const SIMCTL_TIMEOUT: Duration = Duration::from_secs(10);
/// Where `CoreSimulator` keeps every device, under the home.
const DEVICES_DIR: &str = "Library/Developer/CoreSimulator/Devices";

/// The `ios-simulators` detector.
pub struct IosSimulators;

impl Detector for IosSimulators {
    fn category(&self) -> Category {
        Category::IosSimulators
    }

    fn detect(&self, context: &DetectContext<'_>) -> Result<Detected, BrozaError> {
        let devices_dir = context.under_home(DEVICES_DIR);
        let mut detected = Detected::default();
        if !context.fs.exists(&devices_dir) {
            return Ok(detected);
        }
        let listing = match list_devices(context) {
            Ok(listing) => listing,
            Err(error) => {
                detected.warnings.push(Detected::unreadable(
                    Category::IosSimulators,
                    &devices_dir,
                    "the simulator devices",
                    &error,
                ));
                return Ok(detected);
            }
        };
        let mut paths: Vec<FindingPath> = listing
            .devices
            .values()
            .flatten()
            .filter(|device| is_unused(device, context))
            .map(|device| {
                let dir = devices_dir.join(&device.udid);
                let bytes = context.node(&dir).map_or(device.data_path_size, |node| node.allocated_bytes);
                path_with(&dir, bytes, device.last_booted())
            })
            .filter(|path| path.size_bytes > 0)
            .collect();
        paths.sort_by(by_size_then_path);
        let builder = start(Category::IosSimulators, "devices", "Unused simulator devices")?
            .description("Simulator devices whose runtime is gone or that have not booted in a long time.")
            .reasoning(
                "Xcode recreates a simulator device on demand; its data is app state from past test runs.",
            );
        Ok(detected.with_finding(finish(builder, paths)?))
    }
}

/// Run `simctl` and parse its answer.
fn list_devices(context: &DetectContext<'_>) -> Result<Listing, BrozaError> {
    let output = context.process.run(XCRUN, &SIMCTL_LIST_ARGS, SIMCTL_TIMEOUT)?;
    if !output.success {
        return Err(BrozaError::Other(format!(
            "`xcrun simctl list` failed: {}",
            String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or_default()
        )));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        BrozaError::Other(format!("`xcrun simctl list -j` is not the JSON Broza expects: {error}"))
    })
}

/// Gone runtime, or last boot older than the threshold; never a device that
/// simply has not been booted yet.
fn is_unused(device: &Device, context: &DetectContext<'_>) -> bool {
    if !device.is_available {
        return true;
    }
    device.last_booted().is_some_and(|at| context.is_unused_since(at))
}

/// `simctl list -j devices`: runtimes to devices.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Listing {
    devices: BTreeMap<String, Vec<Device>>,
}

/// One simulator device, the fields Broza reads.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Device {
    udid: String,
    is_available: bool,
    data_path_size: u64,
    last_booted_at: Option<String>,
    #[allow(dead_code)]
    data_path: Option<PathBuf>,
}

impl Device {
    /// `lastBootedAt` as a timestamp, when present and well-formed.
    fn last_booted(&self) -> Option<Timestamp> {
        self.last_booted_at.as_deref().and_then(|raw| raw.parse().ok())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::detect::detector::LOCATION_UNREADABLE_CODE;
    use crate::detect::test_support::{context_over, home};
    use crate::model::{Action, Risk};
    use crate::ports::ProcessOutput;
    use crate::testing::{FakeFileOps, FakeRunner};

    const H: &str = "/System/Volumes/Data/Users/dana";
    const OLD_RUNTIME: &str = "7A86C94C-CFEE-4459-A0BC-2C88318F1057";
    const RECENT: &str = "1B2C3D4E-0000-4000-8000-000000000001";
    const STALE: &str = "1B2C3D4E-0000-4000-8000-000000000002";
    const NEVER_BOOTED: &str = "1B2C3D4E-0000-4000-8000-000000000003";

    fn fs() -> FakeFileOps {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        for (udid, size) in [
            (OLD_RUNTIME, 13_000_000_u64),
            (RECENT, 2_000_000_000),
            (STALE, 5_000_000_000),
            (NEVER_BOOTED, 13_000_000),
        ] {
            let file = format!("{H}/Library/Developer/CoreSimulator/Devices/{udid}/data/blob");
            fs.add_file(&file, &[]);
            fs.set_size(&file, size);
        }
        fs
    }

    fn runner_with_fixture() -> FakeRunner {
        FakeRunner::new()
            .with_fixture(XCRUN, &SIMCTL_LIST_ARGS, "plist/macos26/simctl_devices.json")
            .unwrap_or_else(|e| panic!("{e}"))
    }

    fn detect(fs: &FakeFileOps, runner: FakeRunner) -> Detected {
        let world = context_over(fs, home()).with_process(runner);
        IosSimulators.detect(&world.context()).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn devices_with_a_gone_runtime_or_a_stale_boot_are_proposed_recent_and_new_ones_are_not() {
        let detected = detect(&fs(), runner_with_fixture());

        assert_eq!(detected.findings.len(), 1, "{detected:?}");
        let finding = &detected.findings[0];
        assert_eq!(finding.id().to_string(), "ios-simulators.devices");
        let udids: Vec<String> = finding
            .paths()
            .iter()
            .map(|p| p.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(udids, vec![STALE.to_owned(), OLD_RUNTIME.to_owned()], "biggest first");
        assert_eq!(finding.action(), Action::Quarantine);
        assert_eq!(finding.risk(), Risk::Amber);
        assert!(finding.paths()[0].path.starts_with(format!("{H}/Library/Developer/CoreSimulator/Devices")));
        assert!(detected.warnings.is_empty(), "{:?}", detected.warnings);
    }

    #[test]
    fn a_failing_xcrun_is_a_warning_and_no_finding() {
        let runner = FakeRunner::new().with_output(
            XCRUN,
            &SIMCTL_LIST_ARGS,
            ProcessOutput {
                success: false,
                code: Some(72),
                stdout: Vec::new(),
                stderr: b"xcrun: error: unable to find utility \"simctl\"".to_vec(),
            },
        );

        let detected = detect(&fs(), runner);

        assert!(detected.findings.is_empty(), "{detected:?}");
        assert_eq!(detected.warnings.len(), 1);
        assert_eq!(detected.warnings[0].code, LOCATION_UNREADABLE_CODE);
        assert!(detected.warnings[0].message.contains("unable to find utility"), "{:?}", detected.warnings);
    }

    #[test]
    fn a_home_without_core_simulator_never_runs_xcrun() {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        fs.add_dir(home());
        let runner = FakeRunner::new();

        let detected = detect(&fs, runner);

        assert!(detected.findings.is_empty() && detected.warnings.is_empty(), "{detected:?}");
    }
}
