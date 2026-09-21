//! Configuration schema: the keys of `docs/cli-spec.md` §3.6 and the
//! `[profiles.<name>]` overrides.
//!
//! Durations and sizes are [`DurationSpec`] and [`ByteSize`]: parsed once at the
//! boundary, and serialised back as their canonical string form (`50MB`, `30d`),
//! so an invalid value can never reach the rest of the program.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::units::{ByteSize, DurationSpec, DurationUnit};

/// Default unused-app threshold, `1y`.
pub const DEFAULT_UNUSED_AFTER: (u64, DurationUnit) = (1, DurationUnit::Years);
/// Default quarantine retention period, `30d`.
pub const DEFAULT_QUARANTINE_TTL: (u64, DurationUnit) = (30, DurationUnit::Days);
/// Default quarantine location, relative to the user's home directory.
pub const DEFAULT_QUARANTINE_PATH: &str = "~/.local/share/broza/quarantine";
/// Default minimum size for findings, `50MB`.
pub const DEFAULT_MIN_SIZE_BYTES: u64 = 50_000_000;
/// Default validity of the scan cache, `24h`.
pub const DEFAULT_CACHE_TTL: (u64, DurationUnit) = (24, DurationUnit::Hours);

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
    /// Unused-app threshold.
    #[serde(default = "default_unused_after")]
    pub unused_after: DurationSpec,
    /// Retention period in quarantine.
    #[serde(default = "default_quarantine_ttl")]
    pub quarantine_ttl: DurationSpec,
    /// Quarantine location; a leading `~` is expanded against the home directory.
    #[serde(default = "default_quarantine_path")]
    pub quarantine_path: String,
    /// Minimum size for findings.
    #[serde(default = "default_min_size")]
    pub min_size: ByteSize,
    /// Whether the Ko-fi support message may be shown (`docs/cli-spec.md` §5).
    #[serde(default = "default_donate_prompt")]
    pub donate_prompt: bool,
    /// Color preference.
    #[serde(default)]
    pub color: ColorChoice,
    /// Permanent exclusion globs.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Validity of the scan cache.
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl: DurationSpec,
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
    pub unused_after: Option<DurationSpec>,
    /// Override for `quarantine-ttl`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine_ttl: Option<DurationSpec>,
    /// Override for `quarantine-path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine_path: Option<String>,
    /// Override for `min-size`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size: Option<ByteSize>,
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
    pub cache_ttl: Option<DurationSpec>,
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

/// Build a fixed duration. The amounts above are small, so the overflow branch
/// of [`DurationSpec::new`] is unreachable; `defaults_match_the_specification_table`
/// proves the values that come out.
fn fixed(spec: (u64, DurationUnit)) -> DurationSpec {
    DurationSpec::new(spec.0, spec.1).unwrap_or_default()
}

fn default_unused_after() -> DurationSpec {
    fixed(DEFAULT_UNUSED_AFTER)
}
fn default_quarantine_ttl() -> DurationSpec {
    fixed(DEFAULT_QUARANTINE_TTL)
}
fn default_quarantine_path() -> String {
    DEFAULT_QUARANTINE_PATH.to_owned()
}
const fn default_min_size() -> ByteSize {
    ByteSize::new(DEFAULT_MIN_SIZE_BYTES)
}
fn default_cache_ttl() -> DurationSpec {
    fixed(DEFAULT_CACHE_TTL)
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
        assert_eq!(config.unused_after.to_string(), "1y");
        assert_eq!(config.quarantine_ttl.to_string(), "30d");
        assert_eq!(config.quarantine_path, "~/.local/share/broza/quarantine");
        assert_eq!(config.min_size.to_string(), "50MB");
        assert_eq!(config.min_size.bytes(), 50_000_000);
        assert!(config.donate_prompt);
        assert_eq!(config.color, ColorChoice::Auto);
        assert!(config.exclude.is_empty());
        assert_eq!(config.cache_ttl.to_string(), "24h");
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
