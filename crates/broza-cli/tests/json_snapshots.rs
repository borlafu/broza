//! Snapshots of the JSON envelope (`docs/cli-spec.md` §4.1).
//!
//! Values that legitimately change on every run — the timestamp and the binary
//! version — are redacted, so the snapshot pins the *shape* of the contract.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;

/// Host pinned so the snapshot never depends on the machine running the tests.
const FIXED_HOST: &str = "26.1/arm64";

fn json_of(args: &[&str]) -> serde_json::Value {
    let home = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let mut command = Command::cargo_bin("broza").unwrap_or_else(|e| panic!("binary: {e}"));
    let output = command
        .env_clear()
        .env("HOME", home.path())
        .env("BROZA_HOST", FIXED_HOST)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("run {args:?}: {e}"));
    assert_eq!(output.status.code(), Some(0), "`broza {args:?}` must exit 0");
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("invalid json: {e}"))
}

#[test]
fn about_json_matches_the_envelope_snapshot() {
    insta::assert_json_snapshot!(json_of(&["about", "--json"]), {
        ".generated_at" => "[rfc3339]",
        ".broza_version" => "[version]",
        ".data.version" => "[version]",
    });
}

#[test]
fn config_list_json_matches_the_envelope_snapshot() {
    insta::assert_json_snapshot!(json_of(&["config", "list", "--json"]), {
        ".generated_at" => "[rfc3339]",
        ".broza_version" => "[version]",
    });
}

#[test]
fn the_timestamp_has_whole_second_precision() {
    let envelope = json_of(&["about", "--json"]);
    let generated_at = envelope["generated_at"].as_str().unwrap_or_else(|| panic!("missing timestamp"));
    assert!(generated_at.ends_with('Z'), "{generated_at} must be UTC");
    assert!(!generated_at.contains('.'), "{generated_at} must have no sub-second digits");
    assert!(
        generated_at.parse::<jiff::Timestamp>().is_ok(),
        "{generated_at} must be a valid RFC 3339 timestamp"
    );
}
