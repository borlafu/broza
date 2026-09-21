//! Exit-code contract of the binary (`docs/cli-spec.md` §2).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

/// A `broza` invocation that never sees the real home directory or the real
/// environment variables of the test runner.
fn broza(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("broza").unwrap_or_else(|e| panic!("binary: {e}"));
    cmd.env_clear().env("HOME", home).env("BROZA_HOST", "26.1/arm64");
    cmd
}

fn temp_home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"))
}

#[test]
fn bare_invocation_prints_help_and_exits_zero() {
    let home = temp_home();
    broza(home.path()).assert().code(0).stdout(contains("broza")).stdout(contains("Commands:"));
}

#[test]
fn about_exits_zero() {
    let home = temp_home();
    broza(home.path()).arg("about").assert().code(0).stdout(contains("ko-fi.com/broza"));
}

#[test]
fn config_list_exits_zero() {
    let home = temp_home();
    broza(home.path())
        .args(["config", "list"])
        .assert()
        .code(0)
        .stdout(contains("unused-after"))
        .stdout(contains("1y"));
}

#[test]
fn unknown_flag_exits_two() {
    let home = temp_home();
    broza(home.path()).arg("--nonsense").assert().code(2);
}

#[test]
fn json_with_csv_exits_two() {
    let home = temp_home();
    broza(home.path())
        .args(["about", "--json", "--csv"])
        .assert()
        .code(2)
        .stderr(contains("--json").and(contains("--csv")));
}

#[test]
fn csv_on_a_command_without_csv_support_exits_two() {
    let home = temp_home();
    broza(home.path()).args(["about", "--csv"]).assert().code(2).stderr(contains("--csv"));
}

#[test]
fn yes_together_with_purge_exits_two() {
    let home = temp_home();
    broza(home.path())
        .args(["clean", "--purge", "--yes"])
        .assert()
        .code(2)
        .stderr(contains("--purge").and(contains("--yes")));
}

#[test]
fn quarantine_purge_with_yes_exits_two() {
    let home = temp_home();
    broza(home.path())
        .args(["quarantine", "purge", "--all", "--yes"])
        .assert()
        .code(2)
        .stderr(contains("PURGE"));
}

#[test]
fn clean_without_a_selection_exits_two() {
    let home = temp_home();
    broza(home.path())
        .arg("clean")
        .assert()
        .code(2)
        .stderr(contains("--category").and(contains("--risk")))
        .stdout(predicates::str::is_empty());
}

#[test]
fn unimplemented_commands_exit_one_with_a_message_on_stderr() {
    let home = temp_home();
    broza(home.path())
        .args(["restore", "--all"])
        .assert()
        .code(1)
        .stderr(contains("not implemented"))
        .stdout(predicates::str::is_empty());
}

#[test]
fn about_json_is_a_valid_envelope() {
    let home = temp_home();
    let output = broza(home.path()).args(["about", "--json"]).output().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(output.status.code(), Some(0));
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("invalid json: {e}"));
    assert_eq!(parsed["schema_version"], "1.1");
    assert_eq!(parsed["command"], "about");
    assert_eq!(parsed["data"]["donate_url"], "https://ko-fi.com/broza");
    assert_eq!(parsed["data"]["license"], "MIT");
    assert_eq!(parsed["data"]["schema_version"], "1.1");
    assert!(parsed["warnings"].is_array());
    assert!(!parsed["generated_at"].as_str().unwrap_or_default().contains('.'));
    assert!(parsed["errors"].is_array());
}

#[test]
fn output_flag_writes_to_a_file_instead_of_stdout() {
    let home = temp_home();
    let target = home.path().join("out.json");
    broza(home.path())
        .args(["about", "--json", "-o"])
        .arg(&target)
        .assert()
        .code(0)
        .stdout(predicates::str::is_empty());
    let written = std::fs::read_to_string(&target).unwrap_or_else(|e| panic!("{e}"));
    assert!(written.contains("\"command\": \"about\""), "{written}");
}

#[test]
fn unwritable_output_path_exits_one() {
    let home = temp_home();
    broza(home.path())
        .args(["about", "-o", "/nonexistent-dir-broza/out.txt"])
        .assert()
        .code(1)
        .stderr(contains("nonexistent-dir-broza"));
}

#[test]
fn invalid_config_file_exits_two() {
    let home = temp_home();
    let config = home.path().join("broken.toml");
    std::fs::write(&config, "unused-aftr = \"1y\"\n").unwrap_or_else(|e| panic!("{e}"));
    broza(home.path())
        .args(["--config"])
        .arg(&config)
        .args(["config", "list"])
        .assert()
        .code(2)
        .stderr(contains("unused-aftr"));
}

#[test]
fn version_reports_the_schema_version() {
    let home = temp_home();
    broza(home.path()).arg("--version").assert().code(0).stdout(contains("JSON schema 1.1"));
}
