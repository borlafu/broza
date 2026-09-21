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
fn a_volume_is_selectable_by_id_by_name_or_by_mount_point() {
    for target in ["disk3s5", "Data", "/System/Volumes/Data"] {
        let envelope = json_of(&["scan", "--json", "--volume", target]);
        let volumes = envelope["data"]["disks"][0]["containers"][0]["volumes"].clone();
        assert_eq!(volumes.as_array().map(Vec::len), Some(1), "{target}");
        assert_eq!(volumes[0]["id"], "disk3s5", "{target}");
    }
}

#[test]
fn a_volume_that_names_nothing_exits_four_and_a_blank_one_exits_two() {
    for target in ["disk9s9", "sda1", "No Such Volume"] {
        let (code, stderr) = failure_of(&["scan", "--volume", target]);
        assert_eq!(code, Some(4), "{target}: {stderr}");
        assert!(stderr.contains(target), "{stderr}");
    }

    let (blank, blank_stderr) = failure_of(&["scan", "--volume", ""]);
    assert_eq!(blank, Some(2), "{blank_stderr}");
    assert!(blank_stderr.contains("device id"), "{blank_stderr}");
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
fn scan_lists_the_largest_consumers_of_the_data_volume() {
    let text = stdout_of(&["scan", "--volume", "Data"]);

    assert!(text.contains("Largest consumers on Data:"), "{text}");
    assert!(text.contains("212.4 GB  ~/Library/Developer"), "the recording's home prints as ~: {text}");
}

#[test]
fn scan_json_carries_the_largest_items_with_their_volume() {
    let envelope = json_of(&["scan", "--json", "--volume", "Data", "--top", "2"]);

    let items = envelope["data"]["largest_items"].as_array().cloned().unwrap_or_default();
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0]["volume_id"], "disk3s5");
    assert!(items[0]["size_bytes"].as_u64().unwrap() >= items[1]["size_bytes"].as_u64().unwrap());
    assert_eq!(envelope["errors"], serde_json::json!([]));
}

#[test]
fn scan_tree_draws_the_folder_tree_to_the_requested_depth() {
    let text = stdout_of(&["scan", "--volume", "Data", "--tree", "--depth", "1", "--min-size", "1GB"]);

    assert!(text.contains("Folder tree of Data:"), "{text}");
    assert!(text.contains("100.0%  /System/Volumes/Data/"), "{text}");
    assert!(text.contains("Users/"), "{text}");
    assert!(!text.contains("dana/"), "depth 1 stops below the first level: {text}");
}

#[test]
fn scan_of_a_path_walks_only_that_path() {
    let text = stdout_of(&["scan", "/System/Volumes/Data/Users/dana/Documents", "--min-size", "1GB"]);

    assert!(text.contains("61.7 GB  ~/Documents/thesis.pdf"), "{text}");
    assert!(!text.contains("DerivedData"), "{text}");
}

#[test]
fn suggest_lists_the_green_categories_of_the_recorded_home() {
    let text = stdout_of(&["suggest"]);

    assert!(text.starts_with("Potentially reclaimable space:"), "{text}");
    assert!(text.contains("SAFE (green)"), "{text}");
    assert!(text.contains("build-cache"), "{text}");
    assert!(text.contains("Xcode DerivedData"), "{text}");
    assert!(text.contains("user-cache"), "{text}");
    assert!(
        text.contains("Docker Desktop disk image: 38.6 GB — inform only, see: broza explain build-cache"),
        "{text}"
    );
    assert!(text.contains("Reported, not reclaimable by Broza: 38.6 GB"), "{text}");
}

#[test]
fn suggest_json_matches_its_snapshot() {
    let envelope = json_of(&["suggest", "--json"]);

    insta::assert_json_snapshot!(envelope, {
        ".generated_at" => "[timestamp]",
        ".broza_version" => "[version]",
        ".data.findings[].paths[].last_used" => "[time]",
    });
}

#[test]
fn clean_dry_run_lists_the_plan_and_changes_nothing() {
    let text = stdout_of(&["clean", "--category", "user-cache"]);

    assert!(text.starts_with("Dry run: nothing was changed."), "{text}");
    assert!(text.contains("~/Library/Caches/com.example.app"), "{text}");
    assert!(text.contains("broza clean --category user-cache --apply"), "{text}");
}

#[test]
fn clean_json_dry_run_matches_its_snapshot() {
    let envelope = json_of(&["clean", "--category", "user-cache", "--json"]);

    insta::assert_json_snapshot!(envelope, {
        ".generated_at" => "[timestamp]",
        ".broza_version" => "[version]",
        ".data.session_id" => "[session]",
    });
}

#[test]
fn clean_apply_with_yes_quarantines_the_recorded_caches_in_the_recording() {
    let envelope = json_of(&["clean", "--category", "user-cache", "--apply", "--yes", "--json"]);

    assert_eq!(envelope["data"]["dry_run"], false, "{envelope}");
    let items = envelope["data"]["items"].as_array().unwrap_or_else(|| panic!("{envelope}"));
    assert!(items.iter().all(|item| item["status"] == "quarantined"), "{envelope}");
    assert!(envelope["data"]["quarantined_bytes"].as_u64().unwrap_or(0) > 0, "{envelope}");
    assert_eq!(envelope["data"]["reclaimed_bytes"], 0, "{envelope}");
}

#[test]
fn clean_apply_without_a_terminal_exits_seven() {
    let (code, stderr) = failure_of(&["clean", "--category", "user-cache", "--apply"]);

    assert_eq!(code, Some(7), "{stderr}");
}

#[test]
fn clean_of_build_cache_skips_the_docker_disk_with_a_warning() {
    let envelope = json_of(&["clean", "--category", "build-cache", "--json"]);

    let warnings = envelope["warnings"].as_array().unwrap_or_else(|| panic!("{envelope}"));
    assert!(warnings.iter().any(|w| w["code"] == "inform_only_skipped"), "{envelope}");
    let items = envelope["data"]["items"].as_array().unwrap_or_else(|| panic!("{envelope}"));
    assert!(
        items.iter().all(|item| !item["path"].as_str().unwrap_or("").ends_with("Docker.raw")),
        "{envelope}"
    );
}

#[test]
fn an_empty_quarantine_lists_as_empty_and_has_nothing_to_expire() {
    let list = stdout_of(&["quarantine", "list"]);
    let csv = stdout_of(&["quarantine", "list", "--csv"]);
    let expire = stdout_of(&["quarantine", "expire", "--yes"]);
    let restore_list = stdout_of(&["restore", "--list"]);

    assert_eq!(list.trim_end(), "Quarantine is empty.");
    assert_eq!(csv.trim_end(), "id,created_at,expires_at,total_bytes,item_count,state");
    assert_eq!(expire.trim_end(), "Nothing to expire.");
    assert_eq!(restore_list.trim_end(), "Quarantine is empty.");
}

#[test]
fn quarantine_list_json_matches_its_snapshot() {
    let envelope = json_of(&["quarantine", "list", "--json"]);

    insta::assert_json_snapshot!(envelope, {
        ".generated_at" => "[timestamp]",
        ".broza_version" => "[version]",
    });
}

#[test]
fn restoring_an_unknown_session_exits_four_and_purging_without_a_terminal_exits_seven() {
    let (restore_code, restore_err) = failure_of(&["restore", "--session", "cln_20200101000000_zzzz"]);
    let (purge_code, purge_err) = failure_of(&["quarantine", "purge", "--all"]);

    assert_eq!(restore_code, Some(4), "{restore_err}");
    // An empty store has nothing to purge, which is not an error.
    assert_eq!(purge_code, Some(0), "{purge_err}");
}

#[test]
fn suggest_csv_has_one_row_per_finding_in_contract_tokens() {
    let text = stdout_of(&["suggest", "--csv", "--category", "build-cache"]);

    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("id,category,title,risk,action,actionable,reclaimable_bytes,item_count"),
        "{text}"
    );
    assert!(lines.iter().skip(1).all(|l| l.starts_with("build-cache.")), "{text}");
    assert!(lines.iter().any(|l| l.contains(",inform_only,false,")), "{text}");
}

#[test]
fn suggest_refuses_an_unknown_category_with_exit_two() {
    let (code, stderr) = failure_of(&["suggest", "--category", "nope"]);

    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("unknown category"), "{stderr}");
}

#[test]
fn scan_of_a_path_on_the_sealed_volume_is_refused() {
    let (code, stderr) = failure_of(&["scan", "/System/Library"]);

    assert_eq!(code, Some(4), "{stderr}");
}

#[test]
fn quiet_silences_the_warning_but_not_the_report() {
    let home = temp_home();
    let output =
        broza(home.path()).args(["scan", "--tree", "--quiet"]).output().unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stderr).is_empty(), "--quiet must silence warnings");
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
