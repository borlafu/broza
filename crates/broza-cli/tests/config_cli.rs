//! End-to-end behaviour of `broza config` (`docs/cli-spec.md` §3.6).
//!
//! Every test runs with a cleared environment and a temporary `HOME`, so the
//! real configuration file is never read or written.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use assert_cmd::Command;
use predicates::str::contains;

fn broza(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("broza").unwrap_or_else(|e| panic!("binary: {e}"));
    cmd.env_clear().env("HOME", home).env("BROZA_HOST", "26.1/arm64");
    cmd
}

fn temp_home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"))
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|e| panic!("mkdir: {e}"));
    }
    std::fs::write(path, body).unwrap_or_else(|e| panic!("write: {e}"));
}

#[test]
fn path_points_inside_the_home_directory_by_default() {
    let home = temp_home();
    let expected = home.path().join(".config/broza/config.toml");
    broza(home.path())
        .args(["config", "path"])
        .assert()
        .code(0)
        .stdout(contains(expected.display().to_string()));
}

#[test]
fn path_follows_the_config_flag() {
    let home = temp_home();
    let custom = home.path().join("elsewhere.toml");
    broza(home.path())
        .args(["--config"])
        .arg(&custom)
        .args(["config", "path"])
        .assert()
        .code(0)
        .stdout(contains(custom.display().to_string()));
}

#[test]
fn path_falls_back_to_the_broza_config_variable() {
    let home = temp_home();
    let from_env = home.path().join("from-env.toml");
    broza(home.path())
        .env("BROZA_CONFIG", &from_env)
        .args(["config", "path"])
        .assert()
        .code(0)
        .stdout(contains(from_env.display().to_string()));
}

#[test]
fn get_reads_a_value_written_to_the_file() {
    let home = temp_home();
    let config = home.path().join("config.toml");
    write(&config, "min-size = \"2GB\"\n");
    broza(home.path())
        .args(["--config"])
        .arg(&config)
        .args(["config", "get", "min-size"])
        .assert()
        .code(0)
        .stdout(contains("2GB"));
}

#[test]
fn get_of_an_unknown_key_exits_two() {
    let home = temp_home();
    broza(home.path())
        .args(["config", "get", "colour"])
        .assert()
        .code(2)
        .stderr(contains("unknown configuration key"));
}

#[test]
fn set_creates_the_default_file_and_survives_a_second_read() {
    let home = temp_home();
    broza(home.path()).args(["config", "set", "min-size", "2GB"]).assert().code(0);

    let written = std::fs::read_to_string(home.path().join(".config/broza/config.toml"))
        .unwrap_or_else(|e| panic!("read back: {e}"));
    assert!(written.contains("min-size = \"2GB\""), "{written}");

    broza(home.path()).args(["config", "get", "min-size"]).assert().code(0).stdout(contains("2GB"));
}

#[test]
fn set_rejects_an_invalid_value_and_writes_nothing() {
    let home = temp_home();
    broza(home.path())
        .args(["config", "set", "unused-after", "soon"])
        .assert()
        .code(2)
        .stderr(contains("invalid duration"));
    assert!(!home.path().join(".config/broza/config.toml").exists());
}

#[test]
fn list_json_uses_the_envelope() {
    let home = temp_home();
    let output =
        broza(home.path()).args(["config", "list", "--json"]).output().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(output.status.code(), Some(0));
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("invalid json: {e}"));
    assert_eq!(parsed["schema_version"], "1.1");
    assert_eq!(parsed["command"], "config");
    assert_eq!(parsed["data"]["quarantine-ttl"], serde_json::json!("30d"));
    assert_eq!(parsed["data"]["donate-prompt"], serde_json::json!(true), "booleans stay booleans");
    assert_eq!(parsed["data"]["exclude"], serde_json::json!([]), "lists stay arrays");
}

#[test]
fn a_profile_overrides_the_file_values() {
    let home = temp_home();
    let config = home.path().join("config.toml");
    write(&config, "min-size = \"500MB\"\n\n[profiles.developer]\nmin-size = \"1GB\"\n");
    broza(home.path())
        .args(["--config"])
        .arg(&config)
        .args(["--profile", "developer", "config", "get", "min-size"])
        .assert()
        .code(0)
        .stdout(contains("1GB"));
}

#[test]
fn an_unknown_profile_exits_two() {
    let home = temp_home();
    broza(home.path())
        .args(["--profile", "ghost", "config", "list"])
        .assert()
        .code(2)
        .stderr(contains("unknown profile"));
}

#[test]
fn no_color_in_the_environment_reaches_the_effective_configuration() {
    let home = temp_home();
    let config = home.path().join("config.toml");
    write(&config, "color = \"always\"\n");
    broza(home.path())
        .env("NO_COLOR", "1")
        .args(["--config"])
        .arg(&config)
        .args(["config", "get", "color"])
        .assert()
        .code(0)
        .stdout(contains("never"));
}

#[test]
fn broza_no_donate_forces_the_donation_key_off() {
    let home = temp_home();
    broza(home.path())
        .env("BROZA_NO_DONATE", "1")
        .args(["config", "get", "donate-prompt"])
        .assert()
        .code(0)
        .stdout(contains("false"));
}

#[test]
fn resetting_everything_without_a_tty_exits_seven() {
    let home = temp_home();
    broza(home.path()).args(["config", "reset"]).assert().code(7);
    assert!(!home.path().join(".config/broza/config.toml").exists());
}

#[test]
fn resetting_everything_with_yes_needs_no_tty() {
    let home = temp_home();
    let config = home.path().join("config.toml");
    write(&config, "# keep\nmin-size = \"2GB\"\n");
    broza(home.path()).args(["--config"]).arg(&config).args(["config", "reset", "--yes"]).assert().code(0);

    let written = std::fs::read_to_string(&config).unwrap_or_else(|e| panic!("{e}"));
    assert!(written.contains("# keep"), "comments survive: {written}");
    assert!(written.contains("min-size = \"50MB\""), "{written}");
}

#[test]
fn a_comment_survives_a_single_key_write() {
    let home = temp_home();
    let config = home.path().join("config.toml");
    write(&config, "# company policy\nunused-after = \"2y\"\n");
    broza(home.path())
        .args(["--config"])
        .arg(&config)
        .args(["config", "set", "min-size", "2GB"])
        .assert()
        .code(0);

    let written = std::fs::read_to_string(&config).unwrap_or_else(|e| panic!("{e}"));
    assert!(written.contains("# company policy"), "{written}");
    assert!(written.contains("unused-after = \"2y\""), "{written}");
    assert!(written.contains("min-size = \"2GB\""), "{written}");
}

#[test]
fn exclude_takes_one_value_per_glob_and_keeps_braces_intact() {
    let home = temp_home();
    let config = home.path().join("config.toml");
    broza(home.path())
        .args(["--config"])
        .arg(&config)
        .args(["config", "set", "exclude", "~/p/**/*.{js,ts}", "~/Library/Caches/com.a,b.*"])
        .assert()
        .code(0);

    broza(home.path())
        .args(["--config"])
        .arg(&config)
        .args(["config", "get", "exclude"])
        .assert()
        .code(0)
        .stdout(contains("~/p/**/*.{js,ts}"))
        .stdout(contains("~/Library/Caches/com.a,b.*"));
}

#[test]
fn get_exclude_json_is_an_array() {
    let home = temp_home();
    let config = home.path().join("config.toml");
    write(&config, "exclude = [\"~/p/**/*.{js,ts}\"]\n");
    let output = broza(home.path())
        .args(["--config"])
        .arg(&config)
        .args(["config", "get", "exclude", "--json"])
        .output()
        .unwrap_or_else(|e| panic!("{e}"));
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(parsed["data"]["exclude"], serde_json::json!(["~/p/**/*.{js,ts}"]));
}

#[test]
fn a_missing_explicit_config_file_exits_two() {
    let home = temp_home();
    broza(home.path())
        .args(["--config"])
        .arg(home.path().join("absent.toml"))
        .args(["config", "list"])
        .assert()
        .code(2)
        .stderr(contains("not found"));
}

#[test]
fn a_read_only_configuration_directory_exits_three() {
    use std::os::unix::fs::PermissionsExt;

    let home = temp_home();
    let locked = home.path().join("locked");
    std::fs::create_dir(&locked).unwrap_or_else(|e| panic!("{e}"));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500))
        .unwrap_or_else(|e| panic!("{e}"));

    broza(home.path())
        .args(["--config"])
        .arg(locked.join("config.toml"))
        .args(["config", "set", "min-size", "2GB"])
        .assert()
        .code(3);

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700))
        .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn an_unreadable_configuration_file_exits_three() {
    use std::os::unix::fs::PermissionsExt;

    let home = temp_home();
    let config = home.path().join("secret.toml");
    write(&config, "min-size = \"2GB\"\n");
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o000))
        .unwrap_or_else(|e| panic!("{e}"));

    broza(home.path()).args(["--config"]).arg(&config).args(["config", "list"]).assert().code(3);

    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600))
        .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn resetting_one_key_rewrites_the_file() {
    let home = temp_home();
    let config = home.path().join("config.toml");
    write(&config, "min-size = \"2GB\"\ncache-ttl = \"1h\"\n");
    broza(home.path()).args(["--config"]).arg(&config).args(["config", "reset", "min-size"]).assert().code(0);

    let written = std::fs::read_to_string(&config).unwrap_or_else(|e| panic!("{e}"));
    assert!(written.contains("min-size = \"50MB\""), "{written}");
    assert!(written.contains("cache-ttl = \"1h\""), "other keys must survive: {written}");
}
