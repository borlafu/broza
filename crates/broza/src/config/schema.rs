//! Configuration schema: the keys of `docs/cli-spec.md` §3.6 and the
//! `[profiles.<name>]` overrides.
//!
//! Durations, sizes and the quarantine path are stored as validated `String`s
//! so the TOML file round-trips exactly what the user wrote; the presentation
//! and scanning layers parse them when they need numbers.
//!
// TODO(M0-A merge): switch `unused_after`, `quarantine_ttl`, `min_size` and
// `cache_ttl` to `model::units::{DurationSpec, ByteSize}` once that module lands.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Default unused-app threshold.
pub const DEFAULT_UNUSED_AFTER: &str = "1y";
/// Default quarantine retention period.
pub const DEFAULT_QUARANTINE_TTL: &str = "30d";
/// Default quarantine location, relative to the user's home directory.
pub const DEFAULT_QUARANTINE_PATH: &str = "~/.local/share/broza/quarantine";
/// Default minimum size for findings.
pub const DEFAULT_MIN_SIZE: &str = "50MB";
/// Default validity of the scan cache.
pub const DEFAULT_CACHE_TTL: &str = "24h";

/// Color preference (`docs/cli-spec.md` §3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ColorChoice {
    /// Color when stdout is an interactive terminal.
    #[default]
    Auto,
    /// Always emit ANSI color.
    Always,
    /// Never emit ANSI color.
    Never,
}

impl ColorChoice {
    /// Stable spelling used in the config file and in `broza config get color`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }

    /// Parse the stable spelling; unknown values are rejected by the caller.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "auto" => Some(Self::Auto),
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            _ => None,
        }
    }
}

/// Effective configuration. Every field maps to one documented key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Config {
    /// Unused-app threshold (duration literal).
    #[serde(default = "default_unused_after")]
    pub unused_after: String,
    /// Retention period in quarantine (duration literal).
    #[serde(default = "default_quarantine_ttl")]
    pub quarantine_ttl: String,
    /// Quarantine location; a leading `~` is expanded against the home directory.
    #[serde(default = "default_quarantine_path")]
    pub quarantine_path: String,
    /// Minimum size for findings (size literal).
    #[serde(default = "default_min_size")]
    pub min_size: String,
    /// Whether the Ko-fi support message may be shown (`docs/cli-spec.md` §5).
    #[serde(default = "default_donate_prompt")]
    pub donate_prompt: bool,
    /// Color preference.
    #[serde(default)]
    pub color: ColorChoice,
    /// Permanent exclusion globs.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Validity of the scan cache (duration literal).
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl: String,
    /// Named profiles, applied with `--profile <name>`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, Profile>,
    /// Categories selected by the active profile, if any. Not a file key.
    #[serde(skip)]
    pub categories: Option<Vec<String>>,
}

/// A `[profiles.<name>]` table: any key of [`Config`] plus `categories`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Profile {
    /// Override for `unused-after`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unused_after: Option<String>,
    /// Override for `quarantine-ttl`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine_ttl: Option<String>,
    /// Override for `quarantine-path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine_path: Option<String>,
    /// Override for `min-size`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size: Option<String>,
    /// Override for `donate-prompt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub donate_prompt: Option<bool>,
    /// Override for `color`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<ColorChoice>,
    /// Override for `exclude`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
    /// Override for `cache-ttl`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_ttl: Option<String>,
    /// Categories this profile restricts commands to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            unused_after: default_unused_after(),
            quarantine_ttl: default_quarantine_ttl(),
            quarantine_path: default_quarantine_path(),
            min_size: default_min_size(),
            donate_prompt: default_donate_prompt(),
            color: ColorChoice::Auto,
            exclude: Vec::new(),
            cache_ttl: default_cache_ttl(),
            profiles: BTreeMap::new(),
            categories: None,
        }
    }
}

impl Config {
    /// Absolute quarantine directory, expanding a leading `~` against `home`.
    pub fn quarantine_dir(&self, home: &Path) -> PathBuf {
        expand_home(&self.quarantine_path, home)
    }
}

/// Expand a leading `~` in `raw` against `home`; other paths pass through.
pub fn expand_home(raw: &str, home: &Path) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if raw == "~" => home.to_path_buf(),
        None => PathBuf::from(raw),
    }
}

fn default_unused_after() -> String {
    DEFAULT_UNUSED_AFTER.to_owned()
}
fn default_quarantine_ttl() -> String {
    DEFAULT_QUARANTINE_TTL.to_owned()
}
fn default_quarantine_path() -> String {
    DEFAULT_QUARANTINE_PATH.to_owned()
}
fn default_min_size() -> String {
    DEFAULT_MIN_SIZE.to_owned()
}
fn default_cache_ttl() -> String {
    DEFAULT_CACHE_TTL.to_owned()
}
const fn default_donate_prompt() -> bool {
    true
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn defaults_match_the_specification_table() {
        let config = Config::default();
        assert_eq!(config.unused_after, "1y");
        assert_eq!(config.quarantine_ttl, "30d");
        assert_eq!(config.quarantine_path, "~/.local/share/broza/quarantine");
        assert_eq!(config.min_size, "50MB");
        assert!(config.donate_prompt);
        assert_eq!(config.color, ColorChoice::Auto);
        assert!(config.exclude.is_empty());
        assert_eq!(config.cache_ttl, "24h");
        assert!(config.profiles.is_empty());
    }

    #[test]
    fn empty_file_deserializes_to_defaults() {
        let parsed: Config = toml::from_str("").unwrap();
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = toml::from_str::<Config>("colour = \"auto\"\n").unwrap_err();
        assert!(err.to_string().contains("colour"), "{err}");
    }

    #[test]
    fn unknown_profile_key_is_rejected() {
        let err = toml::from_str::<Config>("[profiles.dev]\nnope = 1\n").unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");
    }

    #[test]
    fn quarantine_dir_expands_tilde() {
        let config = Config::default();
        assert_eq!(
            config.quarantine_dir(Path::new("/Users/test")),
            PathBuf::from("/Users/test/.local/share/broza/quarantine")
        );
    }

    #[test]
    fn absolute_quarantine_path_is_left_alone() {
        let config = Config { quarantine_path: "/Volumes/Ext/.q".into(), ..Config::default() };
        assert_eq!(config.quarantine_dir(Path::new("/Users/test")), PathBuf::from("/Volumes/Ext/.q"));
    }

    #[test]
    fn color_choice_round_trips_through_its_stable_spelling() {
        for choice in [ColorChoice::Auto, ColorChoice::Always, ColorChoice::Never] {
            assert_eq!(ColorChoice::parse(choice.as_str()), Some(choice));
        }
        assert_eq!(ColorChoice::parse("rainbow"), None);
    }

    #[test]
    fn serializing_defaults_omits_empty_profiles() {
        let rendered = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(!rendered.contains("profiles"), "{rendered}");
        assert!(rendered.contains("unused-after = \"1y\""), "{rendered}");
    }
}
