//! Key-addressed access to the configuration (`broza config get|set|list|reset`).
//!
//! Keys are the `kebab-case` names of `docs/cli-spec.md` §3.6. Unknown keys are
//! a usage error (exit `2`). Every function returns a new [`Config`]; nothing is
//! mutated in place.
//!
//! Values keep their type: `exclude` is a list of globs and is never joined or
//! split on a separator, so patterns containing `,` — `~/p/**/*.{js,ts}` — round
//! trip unchanged.

use std::str::FromStr;

use serde_json::Value as Json;

use crate::BrozaError;
use crate::config::schema::{ColorChoice, Config};
use crate::units::{ByteSize, DurationSpec};

/// Every recognised key, in the order of the specification table.
pub const KEYS: [&str; 8] = [
    "unused-after",
    "quarantine-ttl",
    "quarantine-path",
    "min-size",
    "donate-prompt",
    "color",
    "exclude",
    "cache-ttl",
];

/// A configuration value with its type preserved.
///
/// Deliberately **not** `#[non_exhaustive]`: it is an internal rendering type,
/// not part of the JSON contract of `model/`, and exhaustive matching in the
/// CLI is what guarantees a new value kind cannot be rendered incorrectly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigValue {
    /// A string-typed key: durations, sizes, paths and `color`.
    Text(String),
    /// A boolean key: `donate-prompt`.
    Flag(bool),
    /// A list key: `exclude`.
    List(Vec<String>),
}

impl ConfigValue {
    /// Human-readable rendering: one list entry per line.
    pub fn to_human(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Flag(flag) => flag.to_string(),
            Self::List(items) => items.join("\n"),
        }
    }

    /// JSON rendering, keeping the type (`docs/cli-spec.md` §4.1).
    pub fn to_json(&self) -> Json {
        match self {
            Self::Text(text) => Json::String(text.clone()),
            Self::Flag(flag) => Json::Bool(*flag),
            Self::List(items) => Json::Array(items.iter().cloned().map(Json::String).collect()),
        }
    }
}

/// Read one key.
///
/// # Errors
///
/// [`BrozaError::Usage`] when `key` is not one of [`KEYS`].
pub fn get(config: &Config, key: &str) -> Result<ConfigValue, BrozaError> {
    match key {
        "unused-after" => Ok(ConfigValue::Text(config.unused_after.to_string())),
        "quarantine-ttl" => Ok(ConfigValue::Text(config.quarantine_ttl.to_string())),
        "quarantine-path" => Ok(ConfigValue::Text(config.quarantine_path.clone())),
        "min-size" => Ok(ConfigValue::Text(config.min_size.to_string())),
        "donate-prompt" => Ok(ConfigValue::Flag(config.donate_prompt)),
        "color" => Ok(ConfigValue::Text(config.color.as_str().to_owned())),
        "exclude" => Ok(ConfigValue::List(config.exclude.clone())),
        "cache-ttl" => Ok(ConfigValue::Text(config.cache_ttl.to_string())),
        other => Err(unknown_key(other)),
    }
}

/// Return a copy of `config` with `key` set to `values`, validated per key type.
///
/// Scalar keys take exactly one value; list keys take one or more. Each value is
/// used verbatim: nothing is split on a separator.
///
/// # Errors
///
/// [`BrozaError::Usage`] when `key` is unknown, the number of values does not
/// match its arity, or a value does not match its type.
pub fn set(config: Config, key: &str, values: &[String]) -> Result<Config, BrozaError> {
    match key {
        "unused-after" => Ok(Config { unused_after: duration(key, values)?, ..config }),
        "quarantine-ttl" => Ok(Config { quarantine_ttl: duration(key, values)?, ..config }),
        "cache-ttl" => Ok(Config { cache_ttl: duration(key, values)?, ..config }),
        "min-size" => Ok(Config { min_size: ByteSize::from_str(scalar(key, values)?)?, ..config }),
        "quarantine-path" => Ok(Config { quarantine_path: validate_path(scalar(key, values)?)?, ..config }),
        "donate-prompt" => Ok(Config { donate_prompt: validate_bool(scalar(key, values)?)?, ..config }),
        "color" => Ok(Config { color: validate_color(scalar(key, values)?)?, ..config }),
        "exclude" => Ok(Config { exclude: validate_globs(key, values)?, ..config }),
        other => Err(unknown_key(other)),
    }
}

/// Return a copy of `config` with `key` — or every key — restored to its default.
///
/// Profiles are preserved: `reset` only touches the top-level keys.
///
/// # Errors
///
/// [`BrozaError::Usage`] when `key` is unknown.
pub fn reset(config: Config, key: Option<&str>) -> Result<Config, BrozaError> {
    let defaults = Config::default();
    let Some(key) = key else {
        return Ok(Config { profiles: config.profiles, ..defaults });
    };
    match key {
        "unused-after" => Ok(Config { unused_after: defaults.unused_after, ..config }),
        "quarantine-ttl" => Ok(Config { quarantine_ttl: defaults.quarantine_ttl, ..config }),
        "quarantine-path" => Ok(Config { quarantine_path: defaults.quarantine_path, ..config }),
        "min-size" => Ok(Config { min_size: defaults.min_size, ..config }),
        "donate-prompt" => Ok(Config { donate_prompt: defaults.donate_prompt, ..config }),
        "color" => Ok(Config { color: defaults.color, ..config }),
        "exclude" => Ok(Config { exclude: defaults.exclude, ..config }),
        "cache-ttl" => Ok(Config { cache_ttl: defaults.cache_ttl, ..config }),
        other => Err(unknown_key(other)),
    }
}

/// All keys with their current values, in specification order.
///
/// # Errors
///
/// [`BrozaError::Usage`] if [`KEYS`] ever names a key [`get`] cannot read.
pub fn list(config: &Config) -> Result<Vec<(String, ConfigValue)>, BrozaError> {
    KEYS.iter().map(|key| get(config, key).map(|value| ((*key).to_owned(), value))).collect()
}

/// Parse the single value of a duration-typed key.
fn duration(key: &str, values: &[String]) -> Result<DurationSpec, BrozaError> {
    DurationSpec::from_str(scalar(key, values)?)
}

/// Parse a boolean literal; `donate-prompt` is the only boolean key.
fn validate_bool(raw: &str) -> Result<bool, BrozaError> {
    match raw.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(BrozaError::Usage(format!("expected `true` or `false`, got `{other}`"))),
    }
}

/// Exactly one value, as scalar keys require.
fn scalar<'a>(key: &str, values: &'a [String]) -> Result<&'a str, BrozaError> {
    match values {
        [single] => Ok(single.as_str()),
        _ => Err(BrozaError::Usage(format!("`{key}` takes exactly one value, got {}", values.len()))),
    }
}

/// One or more non-empty glob patterns, kept verbatim.
fn validate_globs(key: &str, values: &[String]) -> Result<Vec<String>, BrozaError> {
    if values.is_empty() {
        return Err(BrozaError::Usage(format!("`{key}` takes at least one value")));
    }
    if values.iter().any(|value| value.trim().is_empty()) {
        return Err(BrozaError::Usage(format!("`{key}` does not accept empty patterns")));
    }
    Ok(values.to_vec())
}

fn validate_color(value: &str) -> Result<ColorChoice, BrozaError> {
    ColorChoice::parse(value).ok_or_else(|| {
        BrozaError::Usage(format!("invalid color `{value}`: expected `auto`, `always` or `never`"))
    })
}

fn validate_path(value: &str) -> Result<String, BrozaError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(BrozaError::Usage("invalid path: value is empty".to_owned()));
    }
    Ok(trimmed.to_owned())
}

fn unknown_key(key: &str) -> BrozaError {
    BrozaError::Usage(format!("unknown configuration key `{key}`: known keys are {}", KEYS.join(", ")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn one(value: &str) -> Vec<String> {
        vec![value.to_owned()]
    }

    #[test]
    fn every_key_is_readable_from_the_defaults() {
        let config = Config::default();
        for key in KEYS {
            assert!(get(&config, key).is_ok(), "{key} must be readable");
        }
    }

    #[test]
    fn list_returns_every_key_in_specification_order() {
        let listed = list(&Config::default()).unwrap();
        assert_eq!(listed.len(), KEYS.len());
        assert_eq!(listed[0], ("unused-after".to_owned(), ConfigValue::Text("1y".to_owned())));
        assert_eq!(listed[7], ("cache-ttl".to_owned(), ConfigValue::Text("24h".to_owned())));
    }

    #[test]
    fn values_keep_their_type_in_json() {
        let config = Config { exclude: vec!["~/a/**".into()], ..Config::default() };
        assert_eq!(get(&config, "donate-prompt").unwrap().to_json(), serde_json::json!(true));
        assert_eq!(get(&config, "exclude").unwrap().to_json(), serde_json::json!(["~/a/**"]));
        assert_eq!(get(&config, "min-size").unwrap().to_json(), serde_json::json!("50MB"));
    }

    #[test]
    fn set_validates_per_key_type() {
        assert!(set(Config::default(), "unused-after", &one("6m")).is_ok());
        assert!(set(Config::default(), "unused-after", &one("6")).is_err());
        assert!(set(Config::default(), "min-size", &one("500MB")).is_ok());
        assert!(set(Config::default(), "min-size", &one("500")).is_err());
        assert!(set(Config::default(), "donate-prompt", &one("false")).is_ok());
        assert!(set(Config::default(), "donate-prompt", &one("0")).is_err());
        assert!(set(Config::default(), "color", &one("never")).is_ok());
        assert!(set(Config::default(), "color", &one("beige")).is_err());
        assert!(set(Config::default(), "quarantine-path", &one("  ")).is_err());
    }

    #[test]
    fn scalar_keys_refuse_more_than_one_value() {
        let values = vec!["1y".to_owned(), "2y".to_owned()];
        let err = set(Config::default(), "unused-after", &values).expect_err("must fail");
        assert!(err.to_string().contains("exactly one value"), "{err}");
        assert!(set(Config::default(), "min-size", &[]).is_err());
    }

    #[test]
    fn set_does_not_mutate_the_original() {
        let original = Config::default();
        let updated = set(original.clone(), "min-size", &one("2GB")).unwrap();
        assert_eq!(original.min_size.bytes(), 50_000_000);
        assert_eq!(updated.min_size.bytes(), 2_000_000_000);
    }

    #[test]
    fn values_are_stored_parsed_and_read_back_in_canonical_form() {
        let updated = set(Config::default(), "min-size", &one("1.5GB")).unwrap();
        assert_eq!(updated.min_size.bytes(), 1_500_000_000);
        assert_eq!(get(&updated, "min-size").unwrap().to_human(), "1500MB");

        let updated = set(Config::default(), "unused-after", &one(" 6m ")).unwrap();
        assert_eq!(get(&updated, "unused-after").unwrap().to_human(), "6m");
    }

    #[test]
    fn the_unit_parsers_reject_non_finite_and_exponent_mantissas() {
        for literal in ["nanGB", "infGB", "infinityTB", "1e3MB", "1E3MB", "+1MB", "1.2.3GB", ".MB"] {
            assert!(set(Config::default(), "min-size", &one(literal)).is_err(), "{literal}");
        }
    }

    /// The units parser is *stricter* than the string validator it replaced:
    /// a mantissa must have digits on both sides of the point.
    #[test]
    fn the_unit_parser_rejects_a_bare_leading_or_trailing_point() {
        for literal in [".5GB", "5.GB"] {
            assert!(set(Config::default(), "min-size", &one(literal)).is_err(), "{literal}");
        }
        assert!(set(Config::default(), "min-size", &one("0.5GB")).is_ok());
    }

    /// Sizes are stored parsed, so reading one back gives the canonical decimal
    /// form rather than the literal the user typed.
    #[test]
    fn binary_units_are_accepted_and_reported_in_decimal_form() {
        let updated = set(Config::default(), "min-size", &one("1GiB")).unwrap();
        assert_eq!(updated.min_size.bytes(), 1_073_741_824);
        assert_eq!(get(&updated, "min-size").unwrap().to_human(), "1073741824B");
    }

    #[test]
    fn the_unit_parsers_reject_malformed_durations() {
        for literal in ["3", "", "d", "1x", "1.5d", "-1d"] {
            assert!(set(Config::default(), "unused-after", &one(literal)).is_err(), "{literal}");
        }
    }

    #[test]
    fn exclude_takes_one_value_per_glob_and_never_splits_on_commas() {
        let globs = vec!["~/p/**/*.{js,ts}".to_owned(), "~/Library/Caches/com.a,b.*".to_owned()];
        let updated = set(Config::default(), "exclude", &globs).unwrap();
        assert_eq!(updated.exclude, globs, "brace and comma globs must survive intact");
        assert_eq!(get(&updated, "exclude").unwrap(), ConfigValue::List(globs.clone()));
        assert_eq!(get(&updated, "exclude").unwrap().to_human(), globs.join("\n"));
    }

    #[test]
    fn exclude_rejects_no_values_and_blank_patterns() {
        assert!(set(Config::default(), "exclude", &[]).is_err());
        assert!(set(Config::default(), "exclude", &one("  ")).is_err());
    }

    #[test]
    fn reset_without_key_restores_defaults_but_keeps_profiles() {
        let config = Config {
            min_size: ByteSize::new(2_000_000_000),
            profiles: [("dev".to_owned(), crate::config::Profile::default())].into_iter().collect(),
            ..Config::default()
        };
        let reverted = reset(config, None).unwrap();
        assert_eq!(reverted.min_size.bytes(), 50_000_000);
        assert!(reverted.profiles.contains_key("dev"));
    }

    #[test]
    fn reset_with_key_only_touches_that_key() {
        let config = Config {
            min_size: ByteSize::new(2_000_000_000),
            cache_ttl: DurationSpec::from_str("1h").unwrap(),
            ..Config::default()
        };
        let reverted = reset(config, Some("min-size")).unwrap();
        assert_eq!(reverted.min_size.bytes(), 50_000_000);
        assert_eq!(reverted.cache_ttl.to_string(), "1h");
    }

    #[test]
    fn unknown_keys_are_usage_errors_everywhere() {
        assert!(matches!(get(&Config::default(), "nope"), Err(BrozaError::Usage(_))));
        assert!(matches!(set(Config::default(), "nope", &one("1")), Err(BrozaError::Usage(_))));
        assert!(matches!(reset(Config::default(), Some("nope")), Err(BrozaError::Usage(_))));
    }
}
