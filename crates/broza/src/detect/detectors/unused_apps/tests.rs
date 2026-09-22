//! Tests of the `unused-apps` detector; the leftovers tests borrow the fixtures.

#![allow(clippy::unwrap_used)]

use jiff::Timestamp;

use super::*;
use crate::detect::spotlight::{MDLS, MDLS_ARGS};
use crate::detect::test_support::{context_over, home};
use crate::ports::ProcessOutput;
use crate::testing::{FakeFileOps, FakeRunner};

pub(super) const H: &str = "/System/Volumes/Data/Users/dana";
const OLD: &str = "/Applications/OldEditor.app";
const FRESH: &str = "/Applications/Fresh.app";
const MYSTERY: &str = "/Applications/Mystery.app";
const NESTED: &str = "/Applications/Utilities/Tool.app";
const LOCAL: &str = "/System/Volumes/Data/Users/dana/Applications/Local.app";
const SYSTEM: &str = "/System/Applications/Mail.app";
pub(super) const LONG_AGO: &str = "2019-08-10T14:00:00Z";
pub(super) const LATELY: &str = "2026-09-10T08:00:00Z";

pub(super) fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

fn info(id: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{id}</string></dict></plist>"#
    )
    .into_bytes()
}

pub(super) fn app(fs: &FakeFileOps, path: &str, id: &str, size: u64, touched: &str) {
    fs.add_file(format!("{path}/Contents/Info.plist"), &info(id));
    fs.add_file(format!("{path}/Contents/MacOS/bin"), &[]);
    fs.set_size(format!("{path}/Contents/MacOS/bin"), size);
    fs.set_times(path, at(touched), at(touched));
}

/// The applications of every test, plus one leftover-looking directory
/// per rule under `~/Library`.
pub(super) fn fs() -> FakeFileOps {
    let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2).with_root("/Applications", 2);
    fs.add_dir(H);
    app(&fs, OLD, "com.old.Editor", 3_000_000_000, LATELY);
    app(&fs, FRESH, "com.fresh.App", 1_000_000_000, LATELY);
    app(&fs, MYSTERY, "org.mystery.App", 500_000_000, LONG_AGO);
    app(&fs, NESTED, "com.nested.Tool", 200_000_000, LATELY);
    app(&fs, LOCAL, "com.local.App", 400_000_000, LATELY);
    app(&fs, SYSTEM, "com.apple.mail", 100_000_000, LONG_AGO);
    // A helper inside the fresh app carries its own identifier.
    fs.add_file(
        format!("{FRESH}/Contents/Frameworks/Fresh Helper.app/Contents/Info.plist"),
        &info("com.fresh.helper"),
    );
    fs
}

pub(super) fn answers(pairs: &[(&str, &str)]) -> FakeRunner {
    let runner = FakeRunner::new();
    for (path, answer) in pairs {
        let args: Vec<&str> = MDLS_ARGS.iter().copied().chain([*path]).collect();
        runner.script_output(
            MDLS,
            &args,
            ProcessOutput {
                success: true,
                code: Some(0),
                stdout: format!("{answer}\n").into_bytes(),
                stderr: Vec::new(),
            },
        );
    }
    runner
}

/// Every application opened this month, so only the leftovers are found.
pub(super) fn everything_recent() -> FakeRunner {
    answers(&[OLD, FRESH, MYSTERY, NESTED, LOCAL].map(|app| (app, "2026-09-01 09:00:00 +0000")))
}

pub(super) fn detect(fs: &FakeFileOps, runner: FakeRunner) -> Detected {
    let world = context_over(fs, home()).with_process(runner);
    UnusedApps.detect(&world.context()).unwrap()
}

pub(super) fn finding<'a>(detected: &'a Detected, id: &str) -> Option<&'a Finding> {
    detected.findings.iter().find(|f| f.id().to_string() == format!("unused-apps.{id}"))
}

pub(super) fn paths(finding: Option<&Finding>) -> Vec<String> {
    finding.map(|f| f.paths().iter().map(|p| p.path.display().to_string()).collect()).unwrap_or_default()
}

#[test]
fn spotlight_dates_sort_the_applications_and_a_missing_date_is_inform_only() {
    let runner = answers(&[
        (OLD, "2024-01-10 09:00:00 +0000"),
        (FRESH, "2026-09-01 09:00:00 +0000"),
        (MYSTERY, "(null)"),
        (NESTED, "2024-02-02 09:00:00 +0000"),
        (LOCAL, "2023-05-05 09:00:00 +0000"),
    ]);

    let detected = detect(&fs(), runner);

    let apps = finding(&detected, "applications");
    assert_eq!(paths(apps), vec![OLD.to_owned(), LOCAL.to_owned(), NESTED.to_owned()], "biggest first");
    assert_eq!((apps.unwrap().risk(), apps.unwrap().action()), (Risk::Amber, Action::Quarantine));
    assert_eq!(apps.unwrap().paths()[0].last_used, Some(at("2024-01-10T09:00:00Z")));
    let unverified = finding(&detected, "unverified").unwrap();
    assert_eq!(paths(Some(unverified)), vec![MYSTERY.to_owned()]);
    assert_eq!(
        (unverified.risk(), unverified.action(), unverified.is_actionable()),
        (Risk::Red, Action::InformOnly, false)
    );
    assert!(
        unverified.instructions().is_some() && unverified.reasoning().unwrap().contains("Low confidence")
    );
    assert!(paths(apps).iter().all(|p| p != SYSTEM), "Apple's are inventory only");
    assert!(detected.warnings.is_empty(), "{:?}", detected.warnings);
}

#[test]
fn a_failing_mdls_is_one_warning_and_every_application_becomes_unverified() {
    let runner = FakeRunner::new();
    for path in [OLD, FRESH, MYSTERY, NESTED, LOCAL] {
        let args: Vec<&str> = MDLS_ARGS.iter().copied().chain([path]).collect();
        runner.script_failure(MDLS, &args, "mdls: command not found");
    }

    let detected = detect(&fs(), runner);

    assert!(finding(&detected, "applications").is_none());
    assert_eq!(
        paths(finding(&detected, "unverified")),
        vec![MYSTERY.to_owned()],
        "only the bundle touched long ago"
    );
    assert_eq!(detected.warnings.len(), 1, "{:?}", detected.warnings);
}

#[test]
fn a_hole_inside_a_bundle_is_a_warning_and_the_application_is_still_sized() {
    let fs = fs();
    fs.add_denied(format!("{MYSTERY}/Contents/Resources"));
    fs.add_file(format!("{MYSTERY}/Contents/Resources/x"), &[]);
    let runner = answers(&[
        (MYSTERY, "(null)"),
        (OLD, "2026-09-01 09:00:00 +0000"),
        (FRESH, "2026-09-01 09:00:00 +0000"),
        (NESTED, "2026-09-01 09:00:00 +0000"),
        (LOCAL, "2026-09-01 09:00:00 +0000"),
    ]);

    let detected = detect(&fs, runner);

    assert_eq!(paths(finding(&detected, "unverified")), vec![MYSTERY.to_owned()]);
    assert!(
        detected.warnings.iter().any(|w| w.message.contains("part of this application")),
        "{:?}",
        detected.warnings
    );
}

#[test]
fn an_unreadable_manifest_is_a_warning_so_its_leftovers_are_not_mistaken_for_orphans() {
    let fs = fs();
    fs.add_denied(format!("{FRESH}/Contents/Info.plist"));

    let detected = detect(&fs, everything_recent());

    assert!(
        detected.warnings.iter().any(|w| w.message.contains("this bundle's identifier")),
        "{:?}",
        detected.warnings
    );
}

#[test]
fn a_mac_without_applications_folders_reports_nothing_and_asks_nothing() {
    let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
    fs.add_dir(H);

    let detected = detect(&fs, FakeRunner::new());

    assert!(detected.findings.is_empty() && detected.warnings.is_empty(), "{detected:?}");
}
