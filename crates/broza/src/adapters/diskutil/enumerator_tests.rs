//! Tests for the enumerator in [`super`]: the commands it runs, the budget
//! it runs them within, and what it makes of a refusal.

use std::sync::Arc;

use super::{
    DISKUTIL, DISKUTIL_TIMEOUT, DiskutilEnumerator, TM_DESTINATIONS_UNAVAILABLE_CODE, classify,
    devices_to_inspect, first_line, parse_apfs_list, parse_list,
};
use crate::BrozaError;
use crate::adapters::tmutil_destinations::{DESTINATION_INFO_ARGS, TMUTIL};
use crate::ports::{DiskEnumerator, FileOps, ProcessOutput, ProcessRunner, SpaceProvider};
use crate::testing::{FakeFileOps, FakeRunner, FakeSpace};

/// A partition map with one physical disk and one container device.
const LIST: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>AllDisksAndPartitions</key>
  <array>
<dict>
  <key>Content</key><string>GUID_partition_scheme</string>
  <key>DeviceIdentifier</key><string>disk0</string>
  <key>Size</key><integer>1000</integer>
</dict>
  </array>
</dict>
</plist>"#;
/// An `apfs list` output without a single container.
const NO_CONTAINERS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Containers</key><array/></dict></plist>"#;
/// What `tmutil` answers on a Mac with Time Machine switched off.
const NO_DESTINATIONS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict></dict></plist>"#;
/// Two containers naming the same unmounted volume.
const REPEATED_VOLUME: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>Containers</key>
  <array>
<dict>
  <key>ContainerReference</key><string>disk3</string>
  <key>Volumes</key>
  <array><dict><key>DeviceIdentifier</key><string>disk3s1</string></dict></array>
</dict>
<dict>
  <key>ContainerReference</key><string>disk4</string>
  <key>Volumes</key>
  <array><dict><key>DeviceIdentifier</key><string>disk3s1</string></dict></array>
</dict>
  </array>
</dict>
</plist>"#;
/// A `diskutil info` output for `disk0`.
const DISK0_INFO: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>DeviceIdentifier</key><string>disk0</string>
  <key>Internal</key><true/>
  <key>MediaName</key><string>APPLE SSD</string>
  <key>TotalSize</key><integer>1000</integer>
</dict>
</plist>"#;

fn ok(stdout: &[u8]) -> ProcessOutput {
    ProcessOutput { success: true, code: Some(0), stdout: stdout.to_vec(), stderr: Vec::new() }
}

fn failure(code: i32, stderr: &str) -> ProcessOutput {
    ProcessOutput { success: false, code: Some(code), stdout: Vec::new(), stderr: stderr.as_bytes().to_vec() }
}

fn enumerator(runner: &Arc<FakeRunner>) -> DiskutilEnumerator {
    DiskutilEnumerator::new(
        Arc::clone(runner) as Arc<dyn ProcessRunner>,
        Arc::new(FakeSpace::new()) as Arc<dyn SpaceProvider>,
        Arc::new(FakeFileOps::new()) as Arc<dyn FileOps>,
    )
}

fn scripted() -> Arc<FakeRunner> {
    Arc::new(
        FakeRunner::new()
            .with_output(DISKUTIL, &["list", "-plist"], ok(LIST))
            .with_output(DISKUTIL, &["apfs", "list", "-plist"], ok(NO_CONTAINERS))
            .with_output(DISKUTIL, &["info", "-plist", "disk0"], ok(DISK0_INFO))
            .with_output(TMUTIL, &DESTINATION_INFO_ARGS, ok(NO_DESTINATIONS)),
    )
}

#[test]
fn every_command_is_the_absolute_path_within_the_enumeration_budget() {
    let runner = scripted();

    let report = enumerator(&runner).enumerate().unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(report.disks.len(), 1);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let calls = runner.calls();
    assert_eq!(calls.len(), 4, "list, apfs list, one info and the Time Machine destinations");
    assert!(calls.iter().all(|call| call.program == DISKUTIL || call.program == TMUTIL));
    assert!(calls.iter().all(|call| call.timeout <= DISKUTIL_TIMEOUT));
    assert!(calls.iter().all(|call| !call.timeout.is_zero()));
    assert_eq!(calls[0].args, vec!["list".to_owned(), "-plist".to_owned()]);
}

#[test]
fn an_enumeration_that_cannot_ask_time_machine_says_so_and_carries_on() {
    let runner = Arc::new(
        FakeRunner::new()
            .with_output(DISKUTIL, &["list", "-plist"], ok(LIST))
            .with_output(DISKUTIL, &["apfs", "list", "-plist"], ok(NO_CONTAINERS))
            .with_output(DISKUTIL, &["info", "-plist", "disk0"], ok(DISK0_INFO))
            .with_failure(TMUTIL, &DESTINATION_INFO_ARGS, "tmutil is not installed"),
    );

    let report = enumerator(&runner).enumerate().unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(report.disks.len(), 1, "the machine is still enumerated");
    assert_eq!(report.warnings.len(), 1);
    assert_eq!(report.warnings[0].code, TM_DESTINATIONS_UNAVAILABLE_CODE);
    assert!(report.warnings[0].message.contains("tmutil is not installed"));
}

#[test]
fn one_device_is_never_inspected_twice() {
    let list = parse_list(LIST).unwrap_or_else(|e| panic!("{e}"));
    let apfs = parse_apfs_list(REPEATED_VOLUME).unwrap_or_else(|e| panic!("{e}"));

    let devices = devices_to_inspect(&list, &apfs);

    assert_eq!(devices, vec!["disk0".to_owned()], "a volume nobody mounted needs no info");
}

#[test]
fn a_diskutil_that_exits_non_zero_is_an_error_and_not_an_empty_machine() {
    let runner = Arc::new(FakeRunner::new().with_output(
        DISKUTIL,
        &["list", "-plist"],
        failure(1, "Unable to run because unable to use the DiskManagement framework\n"),
    ));

    let err = enumerator(&runner).enumerate().err();

    let Some(BrozaError::Other(message)) = err else { panic!("expected BrozaError::Other") };
    assert!(message.contains("diskutil list -plist"), "{message}");
    assert!(message.contains("status 1"), "{message}");
    assert!(message.contains("DiskManagement"), "{message}");
}

#[test]
fn a_target_diskutil_cannot_find_is_a_not_found_error() {
    let err = classify(&["info", "-plist", "disk9"], Some(1), "Could not find disk: disk9");

    assert!(matches!(err, BrozaError::TargetNotFound(message) if message.contains("disk9")));
}

#[test]
fn a_refusal_is_a_permission_error_whatever_the_wording() {
    for reason in [
        "Permission denied",
        "Operation not permitted",
        "You must have authorization to do that",
        "insufficient privileges",
    ] {
        let err = classify(&["list", "-plist"], Some(1), reason);

        assert!(matches!(err, BrozaError::PermissionDenied { .. }), "{reason}: {err:?}");
    }
}

#[test]
fn anything_else_keeps_the_text_diskutil_printed() {
    let err = classify(&["list", "-plist"], None, "something new");

    let BrozaError::Other(message) = err else { panic!("expected BrozaError::Other") };
    assert!(message.contains("something new"), "{message}");
    assert!(!message.contains("status"), "no code, no status: {message}");
}

#[test]
fn a_diskutil_that_cannot_be_run_at_all_is_reported_as_it_is() {
    let runner = Arc::new(FakeRunner::new().with_failure(DISKUTIL, &["list", "-plist"], "timed out"));

    let err = enumerator(&runner).enumerate().err();

    assert!(matches!(err, Some(BrozaError::Other(message)) if message == "timed out"));
}

#[test]
fn output_that_is_not_a_plist_names_the_command_that_produced_it() {
    let runner = Arc::new(FakeRunner::new().with_output(DISKUTIL, &["list", "-plist"], ok(b"<broken")));

    let err = enumerator(&runner).enumerate().err();

    let Some(BrozaError::Other(message)) = err else { panic!("expected BrozaError::Other") };
    assert!(message.contains("diskutil list -plist"), "{message}");
}

#[test]
fn a_failing_info_stops_the_enumeration_rather_than_inventing_a_disk() {
    let runner =
        Arc::new(FakeRunner::new().with_output(DISKUTIL, &["list", "-plist"], ok(LIST)).with_output(
            DISKUTIL,
            &["apfs", "list", "-plist"],
            ok(NO_CONTAINERS),
        ));

    let err = enumerator(&runner).enumerate().err();

    assert!(err.is_some(), "an info Broza cannot read must not become a disk with no model");
}

#[test]
fn only_the_first_line_of_a_complaint_is_reported() {
    assert_eq!(first_line("\n  boom  \nusage: diskutil\n"), "boom");
    assert_eq!(first_line(""), "");
}
