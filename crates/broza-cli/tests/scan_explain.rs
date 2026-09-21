//! `broza scan` and `broza explain` end to end, against a recorded machine.
//!
//! The binary is driven with `BROZA_FAKE_DISKUTIL_FIXTURES` pointing at the
//! macOS 26 plists in `crates/broza/tests/fixtures/plist/macos26`, so these
//! tests exercise the whole pipeline — clap, wiring, the `diskutil` adapters,
//! the renderers — without spawning `diskutil` or reading a real disk
//! (`AGENTS.md` §7). `BROZA_HOST` pins the host block for the same reason.
//!
//! The seam is debug-only and behind the `fake-diskutil` feature, so without
//! it this file compiles to nothing.
#![cfg(feature = "fake-diskutil")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use assert_cmd::Command;

/// The recorded machine these tests replay.
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../broza/tests/fixtures/plist/macos26");
/// Host pinned so no snapshot depends on the machine running the tests.
const FIXED_HOST: &str = "26.1/arm64";
fn temp_home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"))
}

/// A `broza` that sees a temporary home and the recorded machine, nothing else.
fn broza(home: &Path) -> Command {
    let mut command = Command::cargo_bin("broza").unwrap_or_else(|e| panic!("binary: {e}"));
    command
        .env_clear()
        .env("HOME", home)
        .env("BROZA_HOST", FIXED_HOST)
        .env("BROZA_FAKE_DISKUTIL_FIXTURES", FIXTURES);
    command
}

/// Standard output of a run that must succeed.
fn stdout_of(args: &[&str]) -> String {
    let home = temp_home();
    let output = broza(home.path()).args(args).output().unwrap_or_else(|e| panic!("run {args:?}: {e}"));
    assert_eq!(output.status.code(), Some(0), "`broza {args:?}` must exit 0");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Exit code and standard error of a run that may fail.
fn failure_of(args: &[&str]) -> (Option<i32>, String) {
    let home = temp_home();
    let output = broza(home.path()).args(args).output().unwrap_or_else(|e| panic!("run {args:?}: {e}"));
    (output.status.code(), String::from_utf8_lossy(&output.stderr).into_owned())
}

fn json_of(args: &[&str]) -> serde_json::Value {
    serde_json::from_str(&stdout_of(args)).unwrap_or_else(|e| panic!("invalid json: {e}"))
}

#[test]
fn scan_human_output_matches_its_snapshot() {
    insta::assert_snapshot!(stdout_of(&["scan"]));
}

#[test]
fn scan_json_matches_the_envelope_snapshot() {
    // The two values that legitimately change on every run are redacted, so
    // the snapshot pins the shape of the contract and nothing else.
    insta::assert_json_snapshot!(json_of(&["scan", "--json"]), {
        ".generated_at" => "[rfc3339]",
        ".broza_version" => "[version]",
    });
}

#[test]
fn scan_csv_matches_its_snapshot() {
    insta::assert_snapshot!(stdout_of(&["scan", "--csv"]));
}

#[test]
fn explain_a_volume_matches_its_snapshot() {
    insta::assert_snapshot!(stdout_of(&["explain", "disk3s5"]));
}

#[test]
fn explain_a_category_short_matches_its_snapshot() {
    insta::assert_snapshot!(stdout_of(&["explain", "cloud-synced", "--short"]));
}

#[test]
fn explain_a_path_json_matches_its_snapshot() {
    insta::assert_json_snapshot!(json_of(&["explain", "/Users", "--json"]), {
        ".generated_at" => "[rfc3339]",
        ".broza_version" => "[version]",
    });
}

#[test]
fn largest_items_is_empty_until_the_walker_lands() {
    let envelope = json_of(&["scan", "--json"]);

    assert_eq!(envelope["data"]["largest_items"], serde_json::json!([]));
    assert!(envelope["data"]["disks"].as_array().is_some_and(|disks| !disks.is_empty()));
}

#[test]
fn purgeable_is_reported_and_never_added_to_free() {
    let envelope = json_of(&["scan", "--json"]);
    let boot = envelope["data"]["disks"][0]["containers"][2].clone();

    let free = boot["free_bytes"].as_u64().unwrap_or_default();
    let purgeable = boot["purgeable_bytes"].as_u64().unwrap_or_default();
    let human = stdout_of(&["scan"]);
    assert!(purgeable > 0, "the recording has purgeable space: {boot}");
    assert!(human.contains("Purgeable"), "{human}");
    assert!(
        !human.contains(&broza_cli::output::format_bytes(free + purgeable)),
        "free and purgeable must never be summed"
    );
}

#[test]
fn a_volume_filter_narrows_the_report_to_one_volume() {
    let envelope = json_of(&["scan", "--json", "--volume", "disk3s5"]);

    let disks = envelope["data"]["disks"].as_array().cloned().unwrap_or_default();
    assert_eq!(disks.len(), 1);
    assert_eq!(disks[0]["containers"].as_array().map(Vec::len), Some(1));
    assert_eq!(disks[0]["containers"][0]["volumes"][0]["id"], "disk3s5");
}

#[test]
fn no_external_drops_the_disk_images() {
    let all = json_of(&["scan", "--json"]);
    let internal = json_of(&["scan", "--json", "--no-external"]);

    let count = |value: &serde_json::Value| value["data"]["disks"].as_array().map_or(0, Vec::len);
    assert_eq!(count(&internal), 1, "only the internal SSD survives");
    assert!(count(&all) > count(&internal), "the recording has external disk images");
}

#[test]
fn an_unknown_volume_exits_four_and_a_malformed_one_exits_two() {
    let (unknown, unknown_stderr) = failure_of(&["scan", "--volume", "disk9s9"]);
    let (malformed, malformed_stderr) = failure_of(&["scan", "--volume", "sda1"]);

    assert_eq!(unknown, Some(4), "{unknown_stderr}");
    assert!(unknown_stderr.contains("disk9s9"), "{unknown_stderr}");
    assert_eq!(malformed, Some(2), "{malformed_stderr}");
    assert!(malformed_stderr.contains("BSD device name"), "{malformed_stderr}");
}

#[test]
fn an_unknown_explain_target_exits_four() {
    for target in ["/nowhere/at/all/that/exists", "not-a-category", "disk9s9", "No Such Volume"] {
        let (code, stderr) = failure_of(&["explain", target]);
        assert_eq!(code, Some(4), "{target}: {stderr}");
        assert!(stderr.contains("broza scan"), "{target}: {stderr}");
    }
}

#[test]
fn csv_is_rejected_on_explain() {
    let (code, stderr) = failure_of(&["explain", "snapshots", "--csv"]);

    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("--csv is not supported by `explain`"), "{stderr}");
}

#[test]
fn the_folder_flags_are_accepted_with_a_note_and_never_an_error() {
    let home = temp_home();
    let output =
        broza(home.path()).args(["scan", "--tree", "--top", "5"]).output().unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("note:"), "{stderr}");
    assert!(stderr.contains("next release"), "{stderr}");
    assert!(stderr.contains("--tree") && stderr.contains("--top"), "{stderr}");
}

#[test]
fn quiet_silences_the_note_but_not_the_report() {
    let home = temp_home();
    let output =
        broza(home.path()).args(["scan", "--tree", "--quiet"]).output().unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stderr).is_empty(), "--quiet must silence notes");
    assert!(!output.stdout.is_empty());
}

#[test]
fn a_path_is_explained_through_the_volume_it_lives_on() {
    let envelope = json_of(&["explain", "/Users", "--json"]);

    assert_eq!(envelope["data"]["kind"], "path");
    assert_eq!(envelope["data"]["path"], "/Users");
    assert!(envelope["data"]["volume"]["id"].is_string());
    assert!(envelope["data"].get("category").is_none(), "{}", envelope["data"]);
}

#[test]
fn a_category_target_wins_over_everything_and_needs_no_disk() {
    let envelope = json_of(&["explain", "cloud-synced", "--json"]);

    assert_eq!(envelope["data"]["kind"], "category");
    assert_eq!(envelope["data"]["category"], "cloud-synced");
    assert_eq!(envelope["data"]["risk"], "red");
    assert_eq!(envelope["data"]["action"], "inform_only");
}

#[test]
fn the_json_of_both_commands_is_a_valid_envelope() {
    for args in [vec!["scan", "--json"], vec!["explain", "disk3s5", "--json"]] {
        let envelope = json_of(&args);
        assert_eq!(envelope["schema_version"], broza::SCHEMA_VERSION, "{args:?}");
        assert_eq!(envelope["host"]["macos_version"], "26.1", "{args:?}");
        assert_eq!(envelope["errors"], serde_json::json!([]), "{args:?}");
    }
}
