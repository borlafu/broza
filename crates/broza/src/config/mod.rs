//! Configuration: schema, layering (defaults < toml < profile < env < flags), keys.
//!
//! The core never reads the process environment: callers pass an [`EnvSnapshot`]
//! built by the CLI.
//!
//! Failure modes, so that each one keeps its own exit code:
//!
//! | Situation | Error | Exit |
//! |---|---|---|
//! | no file at the **implicit** `~/.config/broza/config.toml` | none, defaults are used | `0` |
//! | no file at an **explicit** `--config` / `BROZA_CONFIG` path | [`BrozaError::Config`] | `2` |
//! | invalid TOML or an unknown key | [`BrozaError::Config`] | `2` |
//! | unreadable file (`EACCES`, `EPERM`) | [`BrozaError::PermissionDenied`] | `3` |
//! | any other I/O failure | [`BrozaError::Io`] | `1` |
//! | `HOME` unset and no explicit path | [`BrozaError::Usage`] | `2` |

pub mod keys;
pub mod layering;
pub mod schema;
mod values;

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::BrozaError;

pub use keys::ConfigValue;
pub use layering::{CliOverrides, DEFAULT_PROFILE, EnvSnapshot, layer};
pub use schema::{ColorChoice, Config, Profile, expand_home};

/// Path of the configuration file relative to the home directory.
const DEFAULT_CONFIG_RELATIVE: &str = ".config/broza/config.toml";

/// Where a configuration path came from, which decides how a missing file is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// `--config` or `BROZA_CONFIG`: the user named this file, so it must exist.
    Explicit,
    /// `~/.config/broza/config.toml`: absence simply means "use the defaults".
    Implicit,
}

/// Resolve which configuration file to use: `--config`, then `BROZA_CONFIG`,
/// then `~/.config/broza/config.toml`.
///
/// # Errors
///
/// [`BrozaError::Usage`] when no path was given and `HOME` is unset.
pub fn resolve_path(explicit: Option<&Path>, env: &EnvSnapshot) -> Result<PathBuf, BrozaError> {
    resolve(explicit, env).map(|(path, _)| path)
}

fn resolve(explicit: Option<&Path>, env: &EnvSnapshot) -> Result<(PathBuf, Origin), BrozaError> {
    if let Some(path) = explicit {
        return Ok((path.to_path_buf(), Origin::Explicit));
    }
    if let Some(path) = env.broza_config.as_ref() {
        return Ok((path.clone(), Origin::Explicit));
    }
    let home = env.home.as_ref().ok_or_else(home_unset)?;
    Ok((default_path(home), Origin::Implicit))
}

/// The error raised whenever an operation needs `HOME` and it is unset.
pub fn home_unset() -> BrozaError {
    BrozaError::Usage(
        "HOME is not set: pass --config <FILE> or set BROZA_CONFIG to name a configuration file".to_owned(),
    )
}

/// Default configuration path for `home`.
pub fn default_path(home: &Path) -> PathBuf {
    home.join(DEFAULT_CONFIG_RELATIVE)
}

/// Load the configuration file, or the built-in defaults when the implicit path
/// has no file.
///
/// Profiles, environment and flags are *not* applied here: call
/// [`layering::layer`] afterwards.
///
/// # Errors
///
/// See the table in the module documentation.
pub fn load(path: Option<&Path>, env: &EnvSnapshot) -> Result<Config, BrozaError> {
    let (resolved, origin) = resolve(path, env)?;
    match read(&resolved)? {
        Some(config) => Ok(config),
        None => missing(&resolved, origin),
    }
}

/// Like [`load`], but a missing file is `Ok(None)` whatever its origin.
///
/// Commands that *create* the file — `config set`, `config reset`,
/// `config path` — use this: naming a file that does not exist yet is the whole
/// point of `--config` for them.
///
/// # Errors
///
/// Everything [`load`] can raise except "explicitly named file not found".
pub fn load_optional(path: Option<&Path>, env: &EnvSnapshot) -> Result<Option<Config>, BrozaError> {
    let (resolved, _) = resolve(path, env)?;
    read(&resolved)
}

/// Read and parse `path`, distinguishing "absent" from "unreadable".
fn read(path: &Path) -> Result<Option<Config>, BrozaError> {
    match std::fs::read_to_string(path) {
        Ok(raw) => parse(&raw, path).map(Some),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(BrozaError::from_io("reading the configuration file", path, source)),
    }
}

/// A missing file is fine at the implicit path and an error at an explicit one.
fn missing(path: &Path, origin: Origin) -> Result<Config, BrozaError> {
    match origin {
        Origin::Implicit => Ok(Config::default()),
        Origin::Explicit => {
            Err(BrozaError::Config(format!("configuration file not found: {}", path.display())))
        }
    }
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

/// Render a configuration back to TOML.
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
    use crate::ExitCode;

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
        let snapshot = env(Path::new("/Users/test")).with_broza_config(Some("/tmp/env.toml".into()));
        assert_eq!(
            resolve_path(Some(Path::new("/tmp/flag.toml")), &snapshot).unwrap(),
            PathBuf::from("/tmp/flag.toml")
        );
        assert_eq!(resolve_path(None, &snapshot).unwrap(), PathBuf::from("/tmp/env.toml"));
        assert_eq!(
            resolve_path(None, &env(Path::new("/Users/test"))).unwrap(),
            PathBuf::from("/Users/test/.config/broza/config.toml")
        );
    }

    #[test]
    fn an_unset_home_is_a_usage_error_only_without_an_explicit_path() {
        let snapshot = EnvSnapshot::default();
        let err = resolve_path(None, &snapshot).expect_err("must fail");
        assert_eq!(ExitCode::from(&err), ExitCode::UsageError);
        assert!(err.to_string().contains("HOME is not set"), "{err}");

        assert!(resolve_path(Some(Path::new("/tmp/x.toml")), &snapshot).is_ok());
        assert!(resolve_path(None, &snapshot.clone().with_broza_config(Some("/tmp/x.toml".into()))).is_ok());
    }

    #[test]
    fn a_missing_implicit_file_yields_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(None, &env(dir.path())).unwrap(), Config::default());
    }

    #[test]
    fn a_missing_explicit_file_is_a_configuration_error() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("absent.toml");
        let err = load(Some(&absent), &env(dir.path())).expect_err("must fail");
        assert_eq!(ExitCode::from(&err), ExitCode::UsageError);
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn load_optional_tolerates_a_missing_explicit_file() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("absent.toml");
        assert_eq!(load_optional(Some(&absent), &env(dir.path())).unwrap(), None);

        let present = dir.path().join("present.toml");
        std::fs::write(&present, "min-size = \"2GB\"\n").unwrap();
        let loaded = load_optional(Some(&present), &env(dir.path())).unwrap().unwrap();
        assert_eq!(loaded.min_size, "2GB");
    }

    #[test]
    fn load_optional_still_reports_an_invalid_file() {
        let dir = tempfile::tempdir().unwrap();
        let broken = dir.path().join("broken.toml");
        std::fs::write(&broken, "min-size = \n").unwrap();
        assert!(load_optional(Some(&broken), &env(dir.path())).is_err());
    }

    #[test]
    fn a_missing_file_named_by_the_environment_is_also_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = env(dir.path()).with_broza_config(Some(dir.path().join("absent.toml")));
        assert!(load(None, &snapshot).is_err());
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
