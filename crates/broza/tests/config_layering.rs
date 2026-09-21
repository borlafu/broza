//! Integration tests for the configuration loader and layering (`docs/cli-spec.md` §1.3, §3.6).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use broza::config::{CliOverrides, ColorChoice, Config, ConfigValue, EnvSnapshot, keys, layering, load};

fn env_with_home(home: &Path) -> EnvSnapshot {
    EnvSnapshot::for_home(home.to_path_buf())
}

fn write_config(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("config.toml");
    std::fs::write(&path, body).unwrap_or_else(|e| panic!("write fixture: {e}"));
    path
}

#[test]
fn missing_file_yields_defaults() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let env = env_with_home(tmp.path());
    let loaded = load(None, &env).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(loaded, Config::default());
}

#[test]
fn spec_example_file_parses() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let path = write_config(
        tmp.path(),
        r#"
unused-after   = "1y"
quarantine-ttl = "30d"
donate-prompt  = true
exclude = ["~/Projects/**/node_modules", "~/Library/Caches/com.mycompany.*"]

[profiles.developer]
min-size   = "500MB"
categories = ["build-cache", "ios-simulators", "duplicates"]
"#,
    );
    let env = env_with_home(tmp.path());
    let loaded = load(Some(&path), &env).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(loaded.exclude.len(), 2);
    assert!(loaded.profiles.contains_key("developer"));
    // Profile values are not applied until layering runs.
    assert_eq!(loaded.min_size, "50MB");
}

#[test]
fn unknown_key_is_a_config_error() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let path = write_config(tmp.path(), "unused-aftr = \"1y\"\n");
    let env = env_with_home(tmp.path());
    let err = load(Some(&path), &env).expect_err("unknown key must fail");
    assert!(matches!(err, broza::BrozaError::Config(_)), "got {err}");
}

#[test]
fn invalid_toml_is_a_config_error() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let path = write_config(tmp.path(), "unused-after = \n");
    let env = env_with_home(tmp.path());
    let err = load(Some(&path), &env).expect_err("invalid toml must fail");
    assert!(matches!(err, broza::BrozaError::Config(_)), "got {err}");
}

#[test]
fn explicit_path_beats_env_which_beats_default() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let from_env = tmp.path().join("env.toml");
    std::fs::write(&from_env, "min-size = \"1GB\"\n").unwrap_or_else(|e| panic!("{e}"));
    let explicit = write_config(tmp.path(), "min-size = \"2GB\"\n");

    let env = env_with_home(tmp.path()).with_broza_config(Some(from_env.clone()));
    assert_eq!(load(None, &env).unwrap_or_else(|e| panic!("{e}")).min_size, "1GB");
    assert_eq!(load(Some(&explicit), &env).unwrap_or_else(|e| panic!("{e}")).min_size, "2GB");
}

#[test]
fn layering_applies_profile_then_env_then_flags() {
    let file = Config {
        min_size: "500MB".into(),
        donate_prompt: true,
        color: ColorChoice::Always,
        ..Config::default()
    };
    let file = Config {
        profiles: [(
            "developer".to_owned(),
            broza::config::Profile { min_size: Some("1GB".into()), ..broza::config::Profile::default() },
        )]
        .into_iter()
        .collect(),
        ..file
    };
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let env = env_with_home(tmp.path()).with_no_color(true).with_broza_no_donate(true);
    let overrides = CliOverrides::default().with_min_size(Some("2GB".into()));

    let merged =
        layering::layer(&file, Some("developer"), &env, &overrides).unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(merged.min_size, "2GB", "flags win over profile");
    assert_eq!(merged.color, ColorChoice::Never, "NO_COLOR forces never");
    assert!(!merged.donate_prompt, "BROZA_NO_DONATE forces false");
    assert_eq!(file.min_size, "500MB", "inputs are never mutated");
}

#[test]
fn unknown_profile_is_an_error() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let env = env_with_home(tmp.path());
    let err = layering::layer(&Config::default(), Some("ghost"), &env, &CliOverrides::default())
        .expect_err("unknown profile must fail");
    assert!(matches!(err, broza::BrozaError::Config(_)), "got {err}");
}

#[test]
fn keys_round_trip_through_set_and_get() {
    let values = vec!["2GB".to_owned()];
    let updated = keys::set(Config::default(), "min-size", &values).unwrap_or_else(|e| panic!("{e}"));
    let read_back = keys::get(&updated, "min-size").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(read_back, ConfigValue::Text("2GB".to_owned()));
    assert_eq!(
        keys::get(&Config::default(), "min-size").unwrap_or_else(|e| panic!("{e}")),
        ConfigValue::Text("50MB".to_owned())
    );

    let reset = keys::reset(updated, Some("min-size")).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(reset.min_size, "50MB");
}

#[test]
fn keys_list_covers_every_documented_key() {
    let listed = keys::list(&Config::default()).unwrap_or_else(|e| panic!("{e}"));
    let names: Vec<&str> = listed.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "unused-after",
            "quarantine-ttl",
            "quarantine-path",
            "min-size",
            "donate-prompt",
            "color",
            "exclude",
            "cache-ttl",
        ]
    );
}

#[test]
fn keys_reject_unknown_names_and_bad_values() {
    let unknown = keys::get(&Config::default(), "nope").expect_err("unknown key");
    assert!(matches!(unknown, broza::BrozaError::Usage(_)), "got {unknown}");

    let bad_duration = keys::set(Config::default(), "unused-after", &["3".to_owned()]).expect_err("no unit");
    assert!(matches!(bad_duration, broza::BrozaError::Usage(_)), "got {bad_duration}");

    let bad_size = keys::set(Config::default(), "min-size", &["big".to_owned()]).expect_err("not a size");
    assert!(matches!(bad_size, broza::BrozaError::Usage(_)), "got {bad_size}");

    let bad_enum = keys::set(Config::default(), "color", &["rainbow".to_owned()]).expect_err("not a color");
    assert!(matches!(bad_enum, broza::BrozaError::Usage(_)), "got {bad_enum}");
}
