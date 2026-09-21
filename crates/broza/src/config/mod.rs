//! Configuration: schema, layering (defaults < toml < profile < env < flags), keys.
//!
//! The core never reads the process environment: callers pass an [`EnvSnapshot`]
//! built by the CLI. A missing configuration file is not an error — it means
//! "use the defaults". An unreadable or invalid file is [`BrozaError::Config`]
//! (exit code `2`).

pub mod keys;
pub mod layering;
pub mod schema;
mod values;

use std::path::{Path, PathBuf};

use crate::BrozaError;

pub use layering::{CliOverrides, DEFAULT_PROFILE, EnvSnapshot, layer};
pub use schema::{ColorChoice, Config, Profile, expand_home};

/// Path of the configuration file relative to the home directory.
const DEFAULT_CONFIG_RELATIVE: &str = ".config/broza/config.toml";

/// Resolve which configuration file to use: `--config`, then `BROZA_CONFIG`,
/// then `~/.config/broza/config.toml`.
pub fn resolve_path(explicit: Option<&Path>, env: &EnvSnapshot) -> PathBuf {
    explicit.map_or_else(
        || env.broza_config.clone().unwrap_or_else(|| default_path(&env.home)),
        Path::to_path_buf,
    )
}

/// Default configuration path for `home`.
pub fn default_path(home: &Path) -> PathBuf {
    home.join(DEFAULT_CONFIG_RELATIVE)
}

/// Load the configuration file, or the built-in defaults when it is absent.
///
/// Profiles, environment and flags are *not* applied here: call
/// [`layering::layer`] afterwards.
///
/// # Errors
///
/// [`BrozaError::Config`] when the file exists but cannot be read, is not valid
/// TOML, or contains an unknown key.
pub fn load(path: Option<&Path>, env: &EnvSnapshot) -> Result<Config, BrozaError> {
    let resolved = resolve_path(path, env);
    if !resolved.exists() {
        return Ok(Config::default());
    }
    let raw = std::fs::read_to_string(&resolved)
        .map_err(|source| BrozaError::Config(format!("cannot read {}: {source}", resolved.display())))?;
    parse(&raw, &resolved)
}

/// Parse TOML into a [`Config`], reporting the file name on failure.
///
/// # Errors
///
/// [`BrozaError::Config`] on invalid TOML or an unknown key.
pub fn parse(raw: &str, origin: &Path) -> Result<Config, BrozaError> {
    toml::from_str(raw)
        .map_err(|source| BrozaError::Config(format!("invalid {}: {source}", origin.display())))
}

/// Render a configuration back to TOML for `broza config set`.
///
/// # Errors
///
/// [`BrozaError::Config`] if the configuration cannot be serialized.
pub fn to_toml(config: &Config) -> Result<String, BrozaError> {
    toml::to_string_pretty(config)
        .map_err(|source| BrozaError::Config(format!("cannot serialize configuration: {source}")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn env(home: &Path) -> EnvSnapshot {
        EnvSnapshot::for_home(home.to_path_buf())
    }

    #[test]
    fn default_path_follows_the_xdg_style_location() {
        assert_eq!(
            default_path(Path::new("/Users/test")),
            PathBuf::from("/Users/test/.config/broza/config.toml")
        );
    }

    #[test]
    fn resolve_path_prefers_the_explicit_flag() {
        let snapshot = EnvSnapshot {
            broza_config: Some(PathBuf::from("/tmp/env.toml")),
            ..env(Path::new("/Users/test"))
        };
        assert_eq!(
            resolve_path(Some(Path::new("/tmp/flag.toml")), &snapshot),
            PathBuf::from("/tmp/flag.toml")
        );
        assert_eq!(resolve_path(None, &snapshot), PathBuf::from("/tmp/env.toml"));
        assert_eq!(
            resolve_path(None, &env(Path::new("/Users/test"))),
            PathBuf::from("/Users/test/.config/broza/config.toml")
        );
    }

    #[test]
    fn parse_reports_the_origin_on_failure() {
        let err = parse("min-size = ", Path::new("/tmp/broken.toml")).expect_err("must fail");
        assert!(err.to_string().contains("/tmp/broken.toml"), "{err}");
    }

    #[test]
    fn to_toml_round_trips_through_parse() {
        let original = Config { min_size: "2GB".into(), exclude: vec!["~/a/**".into()], ..Config::default() };
        let rendered = to_toml(&original).unwrap();
        let reparsed = parse(&rendered, Path::new("/tmp/x.toml")).unwrap();
        assert_eq!(reparsed, original);
    }
}
