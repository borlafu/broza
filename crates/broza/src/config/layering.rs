//! Pure configuration layering (`docs/cli-spec.md` §1.3).
//!
//! ```text
//! built-in defaults  <  config.toml  <  profile (--profile)  <  environment  <  command-line flags
//! ```
//!
//! Every function here is pure: inputs are borrowed, a brand-new [`Config`] is
//! returned, and nothing is mutated in place.
//!
//! # Environment variables
//!
//! | Variable | Key it overrides | Effect |
//! |---|---|---|
//! | `NO_COLOR` | `color` | forces `never` |
//! | `BROZA_NO_DONATE` | `donate-prompt` | forces `false` |
//! | `CI` | none | non-interactive run: the caller suppresses prompts, progress and the donation message |
//! | `BROZA_CONFIG` | none | selects *which* file is loaded (see [`crate::config::load`]) |

use std::path::PathBuf;

use crate::BrozaError;
use crate::config::schema::{ColorChoice, Config, Profile};
use crate::units::{ByteSize, DurationSpec};

/// Values that command-line flags may override. `None` means "flag absent".
///
/// Build with [`CliOverrides::default`] and the `with_*` methods; the struct is
/// `#[non_exhaustive]` so new flags do not break downstream construction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CliOverrides {
    /// `--unused-after`.
    pub unused_after: Option<DurationSpec>,
    /// `--min-size`.
    pub min_size: Option<ByteSize>,
    /// `--no-color`, mapped to [`ColorChoice::Never`].
    pub no_color: bool,
    /// Extra `--exclude` globs, merged after the configured ones.
    pub exclude: Vec<String>,
    /// `--category`, restricting the run to these categories.
    pub categories: Option<Vec<String>>,
}

impl CliOverrides {
    /// Set `--unused-after`.
    #[must_use]
    pub fn with_unused_after(self, value: Option<DurationSpec>) -> Self {
        Self { unused_after: value, ..self }
    }

    /// Set `--min-size`.
    #[must_use]
    pub fn with_min_size(self, value: Option<ByteSize>) -> Self {
        Self { min_size: value, ..self }
    }

    /// Set `--no-color`.
    #[must_use]
    pub fn with_no_color(self, value: bool) -> Self {
        Self { no_color: value, ..self }
    }

    /// Set the extra `--exclude` globs.
    #[must_use]
    pub fn with_exclude(self, value: Vec<String>) -> Self {
        Self { exclude: value, ..self }
    }

    /// Set `--category`.
    #[must_use]
    pub fn with_categories(self, value: Option<Vec<String>>) -> Self {
        Self { categories: value, ..self }
    }
}

/// Environment facts the core is allowed to see. Built by the CLI; never read
/// from the process environment inside this crate.
///
/// Build with [`EnvSnapshot::default`] or [`EnvSnapshot::for_home`] and the
/// `with_*` methods.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct EnvSnapshot {
    /// Home directory used to resolve `~` and the default config path.
    /// `None` when `HOME` is unset: any operation that needs it fails loudly.
    pub home: Option<PathBuf>,
    /// `NO_COLOR` is set.
    pub no_color: bool,
    /// `BROZA_NO_DONATE` is set.
    pub broza_no_donate: bool,
    /// `CI` is set: the run is non-interactive.
    pub ci: bool,
    /// `BROZA_CONFIG`, lower priority than `--config`.
    pub broza_config: Option<PathBuf>,
}

impl EnvSnapshot {
    /// A snapshot with `home` and no environment variables set.
    pub fn for_home(home: PathBuf) -> Self {
        Self { home: Some(home), ..Self::default() }
    }

    /// Set the home directory, or clear it to model an unset `HOME`.
    #[must_use]
    pub fn with_home(self, home: Option<PathBuf>) -> Self {
        Self { home, ..self }
    }

    /// Set `NO_COLOR`.
    #[must_use]
    pub fn with_no_color(self, value: bool) -> Self {
        Self { no_color: value, ..self }
    }

    /// Set `BROZA_NO_DONATE`.
    #[must_use]
    pub fn with_broza_no_donate(self, value: bool) -> Self {
        Self { broza_no_donate: value, ..self }
    }

    /// Set `CI`.
    #[must_use]
    pub fn with_ci(self, value: bool) -> Self {
        Self { ci: value, ..self }
    }

    /// Set `BROZA_CONFIG`.
    #[must_use]
    pub fn with_broza_config(self, value: Option<PathBuf>) -> Self {
        Self { broza_config: value, ..self }
    }
}

/// Merge `file` with the selected `profile`, the environment and the CLI flags.
///
/// # Errors
///
/// [`BrozaError::Config`] when `profile` names a profile the file does not define.
pub fn layer(
    file: &Config,
    profile: Option<&str>,
    env: &EnvSnapshot,
    cli: &CliOverrides,
) -> Result<Config, BrozaError> {
    let with_profile = apply_profile(file, profile)?;
    let with_env = apply_env(&with_profile, env);
    Ok(apply_cli(&with_env, cli))
}

/// Apply `[profiles.<name>]` on top of the file values.
fn apply_profile(file: &Config, profile: Option<&str>) -> Result<Config, BrozaError> {
    let Some(name) = profile else {
        return Ok(file.clone());
    };
    if name == DEFAULT_PROFILE && !file.profiles.contains_key(DEFAULT_PROFILE) {
        return Ok(file.clone());
    }
    let Some(overrides) = file.profiles.get(name) else {
        return Err(BrozaError::Config(format!(
            "unknown profile `{name}`: define [profiles.{name}] in the configuration file"
        )));
    };
    Ok(merge_profile(file, overrides))
}

/// The implicit profile name; absent from the file it simply means "no profile".
pub const DEFAULT_PROFILE: &str = "default";

fn merge_profile(base: &Config, overrides: &Profile) -> Config {
    Config {
        unused_after: overrides.unused_after.unwrap_or(base.unused_after),
        quarantine_ttl: overrides.quarantine_ttl.unwrap_or(base.quarantine_ttl),
        quarantine_path: pick(&base.quarantine_path, overrides.quarantine_path.as_ref()),
        min_size: overrides.min_size.unwrap_or(base.min_size),
        donate_prompt: overrides.donate_prompt.unwrap_or(base.donate_prompt),
        color: overrides.color.unwrap_or(base.color),
        exclude: overrides.exclude.clone().unwrap_or_else(|| base.exclude.clone()),
        cache_ttl: overrides.cache_ttl.unwrap_or(base.cache_ttl),
        profiles: base.profiles.clone(),
        categories: overrides.categories.clone().or_else(|| base.categories.clone()),
    }
}

/// Apply the environment variables documented in the module header.
fn apply_env(base: &Config, env: &EnvSnapshot) -> Config {
    Config {
        color: if env.no_color { ColorChoice::Never } else { base.color },
        donate_prompt: base.donate_prompt && !env.broza_no_donate,
        ..base.clone()
    }
}

/// Apply the command-line flags, the highest-priority layer.
fn apply_cli(base: &Config, cli: &CliOverrides) -> Config {
    let exclude = if cli.exclude.is_empty() {
        base.exclude.clone()
    } else {
        let mut merged = base.exclude.clone();
        merged.extend(cli.exclude.iter().cloned());
        merged
    };
    Config {
        unused_after: cli.unused_after.unwrap_or(base.unused_after),
        min_size: cli.min_size.unwrap_or(base.min_size),
        color: if cli.no_color { ColorChoice::Never } else { base.color },
        exclude,
        categories: cli.categories.clone().or_else(|| base.categories.clone()),
        ..base.clone()
    }
}

fn pick(base: &str, override_value: Option<&String>) -> String {
    override_value.map_or_else(|| base.to_owned(), Clone::clone)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn env() -> EnvSnapshot {
        EnvSnapshot::for_home(PathBuf::from("/Users/test"))
    }

    fn size(raw: &str) -> ByteSize {
        raw.parse().unwrap_or_else(|e| panic!("{raw}: {e}"))
    }

    fn file_with_profile() -> Config {
        Config {
            min_size: size("500MB"),
            profiles: [(
                "developer".to_owned(),
                Profile {
                    min_size: Some(size("1GB")),
                    categories: Some(vec!["build-cache".into()]),
                    ..Profile::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Config::default()
        }
    }

    #[test]
    fn without_profile_the_file_values_survive() {
        let merged = layer(&file_with_profile(), None, &env(), &CliOverrides::default()).unwrap();
        assert_eq!(merged.min_size, size("500MB"));
        assert_eq!(merged.categories, None);
    }

    #[test]
    fn profile_overrides_the_file() {
        let merged =
            layer(&file_with_profile(), Some("developer"), &env(), &CliOverrides::default()).unwrap();
        assert_eq!(merged.min_size, size("1GB"));
        assert_eq!(merged.categories, Some(vec!["build-cache".to_owned()]));
    }

    #[test]
    fn implicit_default_profile_is_not_an_error_when_undefined() {
        let merged = layer(&Config::default(), Some(DEFAULT_PROFILE), &env(), &CliOverrides::default());
        assert!(merged.is_ok(), "the implicit default profile must be tolerated");
    }

    #[test]
    fn no_color_env_forces_never_even_over_always_in_file() {
        let file = Config { color: ColorChoice::Always, ..Config::default() };
        let snapshot = env().with_no_color(true);
        let merged = layer(&file, None, &snapshot, &CliOverrides::default()).unwrap();
        assert_eq!(merged.color, ColorChoice::Never);
    }

    #[test]
    fn broza_no_donate_env_forces_donate_prompt_off() {
        let snapshot = env().with_broza_no_donate(true);
        let merged = layer(&Config::default(), None, &snapshot, &CliOverrides::default()).unwrap();
        assert!(!merged.donate_prompt);
    }

    #[test]
    fn ci_alone_changes_no_configuration_key() {
        let snapshot = env().with_ci(true);
        let merged = layer(&Config::default(), None, &snapshot, &CliOverrides::default()).unwrap();
        assert_eq!(merged, Config::default(), "CI only affects interactivity, not keys");
    }

    #[test]
    fn cli_flags_win_over_everything_else() {
        let snapshot = env().with_no_color(false);
        let overrides = CliOverrides::default()
            .with_min_size(Some(size("2GB")))
            .with_unused_after(Some("6m".parse().unwrap_or_default()))
            .with_no_color(true);
        let merged = layer(&file_with_profile(), Some("developer"), &snapshot, &overrides).unwrap();
        assert_eq!(merged.min_size, size("2GB"));
        assert_eq!(merged.unused_after.to_string(), "6m");
        assert_eq!(merged.color, ColorChoice::Never);
    }

    #[test]
    fn cli_exclusions_are_merged_with_configured_ones() {
        let file = Config { exclude: vec!["~/a/**".into()], ..Config::default() };
        let overrides = CliOverrides::default().with_exclude(vec!["~/b/**".into()]);
        let merged = layer(&file, None, &env(), &overrides).unwrap();
        assert_eq!(merged.exclude, vec!["~/a/**".to_owned(), "~/b/**".to_owned()]);
        assert_eq!(file.exclude, vec!["~/a/**".to_owned()], "input untouched");
    }

    #[test]
    fn unknown_profile_is_rejected() {
        let err = layer(&Config::default(), Some("ghost"), &env(), &CliOverrides::default())
            .expect_err("must fail");
        assert!(matches!(err, BrozaError::Config(_)), "{err}");
    }
}
