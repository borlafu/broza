//! Key-addressed access to the configuration (`broza config get|set|list|reset`).
//!
//! Keys are the `kebab-case` names of `docs/cli-spec.md` §3.6. Unknown keys are
//! a usage error (exit `2`). Every function returns a new [`Config`]; nothing is
//! mutated in place.

use crate::BrozaError;
use crate::config::schema::{ColorChoice, Config};
use crate::config::values::{validate_bool, validate_duration, validate_size};

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

/// Separator used to render and parse the `exclude` list on the command line.
const LIST_SEPARATOR: &str = ",";

/// Read one key as the string `broza config get` prints.
///
/// # Errors
///
/// [`BrozaError::Usage`] when `key` is not one of [`KEYS`].
pub fn get(config: &Config, key: &str) -> Result<String, BrozaError> {
    match key {
        "unused-after" => Ok(config.unused_after.clone()),
        "quarantine-ttl" => Ok(config.quarantine_ttl.clone()),
        "quarantine-path" => Ok(config.quarantine_path.clone()),
        "min-size" => Ok(config.min_size.clone()),
        "donate-prompt" => Ok(config.donate_prompt.to_string()),
        "color" => Ok(config.color.as_str().to_owned()),
        "exclude" => Ok(config.exclude.join(LIST_SEPARATOR)),
        "cache-ttl" => Ok(config.cache_ttl.clone()),
        other => Err(unknown_key(other)),
    }
}

/// Return a copy of `config` with `key` set to `value`, validated per key type.
///
/// # Errors
///
/// [`BrozaError::Usage`] when `key` is unknown or `value` does not match its type.
pub fn set(config: Config, key: &str, value: &str) -> Result<Config, BrozaError> {
    match key {
        "unused-after" => Ok(Config { unused_after: validate_duration(value)?, ..config }),
        "quarantine-ttl" => Ok(Config { quarantine_ttl: validate_duration(value)?, ..config }),
        "cache-ttl" => Ok(Config { cache_ttl: validate_duration(value)?, ..config }),
        "min-size" => Ok(Config { min_size: validate_size(value)?, ..config }),
        "quarantine-path" => Ok(Config { quarantine_path: validate_path(value)?, ..config }),
        "donate-prompt" => Ok(Config { donate_prompt: validate_bool(value)?, ..config }),
        "color" => Ok(Config { color: validate_color(value)?, ..config }),
        "exclude" => Ok(Config { exclude: parse_list(value), ..config }),
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
pub fn list(config: &Config) -> Vec<(String, String)> {
    KEYS.iter().filter_map(|key| get(config, key).ok().map(|value| ((*key).to_owned(), value))).collect()
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

fn parse_list(value: &str) -> Vec<String> {
    value
        .split(LIST_SEPARATOR)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn unknown_key(key: &str) -> BrozaError {
    BrozaError::Usage(format!("unknown configuration key `{key}`: known keys are {}", KEYS.join(", ")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn every_key_is_readable_from_the_defaults() {
        let config = Config::default();
        for key in KEYS {
            assert!(get(&config, key).is_ok(), "{key} must be readable");
        }
    }

    #[test]
    fn list_returns_every_key_in_specification_order() {
        let listed = list(&Config::default());
        assert_eq!(listed.len(), KEYS.len());
        assert_eq!(listed[0], ("unused-after".to_owned(), "1y".to_owned()));
        assert_eq!(listed[7], ("cache-ttl".to_owned(), "24h".to_owned()));
    }

    #[test]
    fn set_validates_per_key_type() {
        assert!(set(Config::default(), "unused-after", "6m").is_ok());
        assert!(set(Config::default(), "unused-after", "6").is_err());
        assert!(set(Config::default(), "min-size", "500MB").is_ok());
        assert!(set(Config::default(), "min-size", "500").is_err());
        assert!(set(Config::default(), "donate-prompt", "false").is_ok());
        assert!(set(Config::default(), "donate-prompt", "0").is_err());
        assert!(set(Config::default(), "color", "never").is_ok());
        assert!(set(Config::default(), "color", "beige").is_err());
        assert!(set(Config::default(), "quarantine-path", "  ").is_err());
    }

    #[test]
    fn set_does_not_mutate_the_original() {
        let original = Config::default();
        let updated = set(original.clone(), "min-size", "2GB").unwrap();
        assert_eq!(original.min_size, "50MB");
        assert_eq!(updated.min_size, "2GB");
    }

    #[test]
    fn exclude_is_stored_as_a_comma_separated_list() {
        let updated = set(Config::default(), "exclude", "~/a/**, ~/b/**,").unwrap();
        assert_eq!(updated.exclude, vec!["~/a/**".to_owned(), "~/b/**".to_owned()]);
        assert_eq!(get(&updated, "exclude").unwrap(), "~/a/**,~/b/**");
    }

    #[test]
    fn reset_without_key_restores_defaults_but_keeps_profiles() {
        let config = Config {
            min_size: "2GB".into(),
            profiles: [("dev".to_owned(), crate::config::Profile::default())].into_iter().collect(),
            ..Config::default()
        };
        let reverted = reset(config, None).unwrap();
        assert_eq!(reverted.min_size, "50MB");
        assert!(reverted.profiles.contains_key("dev"));
    }

    #[test]
    fn reset_with_key_only_touches_that_key() {
        let config = Config { min_size: "2GB".into(), cache_ttl: "1h".into(), ..Config::default() };
        let reverted = reset(config, Some("min-size")).unwrap();
        assert_eq!(reverted.min_size, "50MB");
        assert_eq!(reverted.cache_ttl, "1h");
    }

    #[test]
    fn unknown_keys_are_usage_errors_everywhere() {
        assert!(matches!(get(&Config::default(), "nope"), Err(BrozaError::Usage(_))));
        assert!(matches!(set(Config::default(), "nope", "1"), Err(BrozaError::Usage(_))));
        assert!(matches!(reset(Config::default(), Some("nope")), Err(BrozaError::Usage(_))));
    }
}
