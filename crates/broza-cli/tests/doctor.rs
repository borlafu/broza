//! `broza doctor` against the real filesystem, under a temporary home.
//!
//! No fixture seam: the probe lists real directories, so a folder made
//! unreadable with `chmod 000` stands in for one macOS keeps private.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use assert_cmd::Command;

fn broza(home: &Path) -> Command {
    let mut command = Command::cargo_bin("broza").unwrap();
    command.env_clear().env("HOME", home).env("BROZA_HOST", "26.1/arm64");
    command
}

fn state_of(home: &Path) -> serde_json::Value {
    let output = broza(home).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stderr));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    json["data"]["full_disk_access"].clone()
}

#[test]
fn a_home_without_protected_folders_cannot_tell_and_still_exits_zero() {
    let home = tempfile::tempdir().unwrap();

    let access = state_of(home.path());

    assert_eq!(access["state"], "unknown");
    assert!(access.get("probe_path").is_none(), "{access}");
}

#[test]
fn a_protected_folder_that_cannot_be_listed_is_missing_access() {
    let home = tempfile::tempdir().unwrap();
    let safari = home.path().join("Library/Safari");
    std::fs::create_dir_all(&safari).unwrap();
    std::fs::set_permissions(&safari, std::fs::Permissions::from_mode(0o000)).unwrap();
    let listable = std::fs::read_dir(&safari).is_ok();

    let access = state_of(home.path());

    std::fs::set_permissions(&safari, std::fs::Permissions::from_mode(0o700)).unwrap();
    if listable {
        eprintln!("skipped: running as root, nothing is refused");
        return;
    }
    assert_eq!(access["state"], "missing", "{access}");
    assert!(access["probe_path"].as_str().unwrap().ends_with("Library/Safari"), "{access}");
}

#[test]
fn doctor_has_no_csv_form() {
    let home = tempfile::tempdir().unwrap();

    broza(home.path()).args(["doctor", "--csv"]).assert().code(2);
}
