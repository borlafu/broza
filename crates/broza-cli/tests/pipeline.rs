//! The pipeline end to end, driven in process through [`broza_cli::run_with_env`].
//!
//! These never touch the real `$HOME`, the real terminal or `sw_vers`: the
//! environment snapshot is built by hand and the host is pinned.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use broza::ExitCode;
use broza_cli::env::RuntimeEnv;
use broza_cli::run_with_env;

/// A pipeline run rooted at a temporary home, writing its data to a file so
/// nothing leaks into the test harness's stdout.
fn run_in(home: &Path, args: &[&str]) -> (ExitCode, String) {
    let target = home.join("captured-output");
    let runtime = RuntimeEnv {
        host_override: Some("26.1/arm64".to_owned()),
        ..RuntimeEnv::for_tests(home.to_path_buf())
    };
    let mut full: Vec<String> = args.iter().map(ToString::to_string).collect();
    full.push("--output".to_owned());
    full.push(target.display().to_string());
    let code = run_with_env(full, &runtime);
    (code, std::fs::read_to_string(&target).unwrap_or_default())
}

fn temp_home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn about_runs_end_to_end() {
    let home = temp_home();
    let (code, text) = run_in(home.path(), &["broza", "about"]);
    assert_eq!(code, ExitCode::Ok);
    assert!(text.contains("ko-fi.com/broza"), "{text}");
}

#[test]
fn about_json_carries_the_injected_host() {
    let home = temp_home();
    let (code, text) = run_in(home.path(), &["broza", "about", "--json"]);
    assert_eq!(code, ExitCode::Ok);
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(parsed["host"]["macos_version"], "26.1");
    assert_eq!(parsed["schema_version"], broza::SCHEMA_VERSION);
    assert_eq!(parsed["warnings"], serde_json::json!([]));
}

#[test]
fn config_runs_against_the_temporary_home() {
    let home = temp_home();
    assert_eq!(run_in(home.path(), &["broza", "config", "set", "min-size", "2GB"]).0, ExitCode::Ok);
    let (code, text) = run_in(home.path(), &["broza", "config", "get", "min-size"]);
    assert_eq!(code, ExitCode::Ok);
    assert_eq!(text.trim(), "2GB");
}

#[test]
fn rejected_flag_combinations_never_reach_a_command() {
    let home = temp_home();
    assert_eq!(run_in(home.path(), &["broza", "about", "--json", "--csv"]).0, ExitCode::UsageError);
    assert_eq!(run_in(home.path(), &["broza", "about", "--csv"]).0, ExitCode::UsageError);
    assert_eq!(run_in(home.path(), &["broza", "clean", "--purge", "--yes"]).0, ExitCode::UsageError);
}

#[test]
fn pending_commands_exit_one() {
    let home = temp_home();
    // `scan`, `explain`, `suggest` and `clean` are covered by `scan_explain.rs`,
    // which drives them against recorded plists. They are deliberately absent
    // here: running them in process would spawn the real `diskutil`. A `clean`
    // without a selection fails before any of that.
    assert_eq!(run_in(home.path(), &["broza", "clean"]).0, ExitCode::UsageError);
    assert_eq!(run_in(home.path(), &["broza", "restore", "--all"]).0, ExitCode::GenericError);
    assert_eq!(run_in(home.path(), &["broza", "quarantine", "list"]).0, ExitCode::GenericError);
}

/// A category explanation is pure text: it enumerates nothing, so it is safe
/// to run in process and must not be mistaken for a pending command.
#[test]
fn explaining_a_category_needs_no_disk_at_all() {
    let home = temp_home();

    let (code, text) = run_in(home.path(), &["broza", "explain", "snapshots"]);

    assert_eq!(code, ExitCode::Ok);
    assert!(text.starts_with("snapshots"), "{text}");
    assert!(text.contains("Is it safe to touch?"), "{text}");
}

#[test]
fn an_invalid_configuration_file_stops_the_pipeline() {
    let home = temp_home();
    let config = home.path().join(".config/broza/config.toml");
    std::fs::create_dir_all(config.parent().unwrap_or_else(|| panic!("parent"))).unwrap();
    std::fs::write(&config, "unused-aftr = \"1y\"\n").unwrap();
    assert_eq!(run_in(home.path(), &["broza", "config", "list"]).0, ExitCode::UsageError);
}

#[test]
fn an_unwritable_output_path_is_a_generic_error() {
    let runtime = RuntimeEnv::for_tests(PathBuf::from("/Users/test"));
    let code = run_with_env(["broza", "about", "--output", "/nonexistent-broza-dir/out.txt"], &runtime);
    assert_eq!(code, ExitCode::GenericError);
}

#[test]
fn an_unset_home_is_a_usage_error_for_commands_that_need_it() {
    let runtime = RuntimeEnv { home: None, ..RuntimeEnv::for_tests(PathBuf::from("/unused")) };
    assert_eq!(run_with_env(["broza", "config", "list"], &runtime), ExitCode::UsageError);
}

#[test]
fn unparseable_arguments_never_reach_the_pipeline() {
    let runtime = RuntimeEnv::for_tests(PathBuf::from("/Users/test"));
    assert_eq!(run_with_env(["broza", "--nope"], &runtime), ExitCode::UsageError);
}

#[test]
fn run_uses_the_real_environment_without_panicking() {
    assert_eq!(broza_cli::run(["broza", "--version"]), ExitCode::Ok);
}

#[test]
fn a_bare_invocation_writes_the_short_help_to_the_sink() {
    let home = temp_home();
    let (code, text) = run_in(home.path(), &["broza"]);
    assert_eq!(code, ExitCode::Ok);
    assert!(text.contains("Commands:"), "--output must capture the help too: {text}");
}
