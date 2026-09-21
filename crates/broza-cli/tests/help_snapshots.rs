//! `--help` snapshots for the whole command tree (AGENTS.md §7).
//!
//! Regenerate with `cargo insta test --accept` after an intentional change to
//! the CLI surface, and review the diff against `docs/cli-spec.md` §3.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;

/// Every `--help` page of the CLI, as argument lists.
const HELP_PAGES: [(&str, &[&str]); 17] = [
    ("root", &[]),
    ("scan", &["scan"]),
    ("explain", &["explain"]),
    ("suggest", &["suggest"]),
    ("clean", &["clean"]),
    ("restore", &["restore"]),
    ("config", &["config"]),
    ("config_get", &["config", "get"]),
    ("config_set", &["config", "set"]),
    ("config_list", &["config", "list"]),
    ("config_path", &["config", "path"]),
    ("config_reset", &["config", "reset"]),
    ("quarantine", &["quarantine"]),
    ("quarantine_list", &["quarantine", "list"]),
    ("quarantine_expire", &["quarantine", "expire"]),
    ("quarantine_purge", &["quarantine", "purge"]),
    ("about", &["about"]),
];

/// A temporary, empty `HOME`: help output must never depend on a real one.
fn temp_home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"))
}

fn broza(home: &std::path::Path) -> Command {
    let mut command = Command::cargo_bin("broza").unwrap_or_else(|e| panic!("binary: {e}"));
    command.env_clear().env("HOME", home).env("TERM", "dumb");
    command
}

fn help_for(args: &[&str]) -> String {
    let home = temp_home();
    let output =
        broza(home.path()).args(args).arg("--help").output().unwrap_or_else(|e| panic!("run {args:?}: {e}"));
    assert_eq!(output.status.code(), Some(0), "`broza {args:?} --help` must exit 0");
    String::from_utf8(output.stdout).unwrap_or_else(|e| panic!("non-UTF-8 help: {e}"))
}

#[test]
fn every_help_page_matches_its_snapshot() {
    for (name, args) in HELP_PAGES {
        insta::assert_snapshot!(format!("help_{name}"), help_for(args));
    }
}

#[test]
fn bare_invocation_prints_the_same_page_as_help() {
    let home = temp_home();
    let output = broza(home.path()).output().unwrap_or_else(|e| panic!("run: {e}"));
    assert_eq!(output.status.code(), Some(0));
    let bare = String::from_utf8(output.stdout).unwrap_or_else(|e| panic!("non-UTF-8: {e}"));
    assert_eq!(bare, help_for(&[]));
}
