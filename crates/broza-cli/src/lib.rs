//! Broza command-line interface: argument parsing, rendering, TTY prompting.
//!
//! [`run`] is the whole pipeline — parse, snapshot the environment, load and
//! layer the configuration, wire the adapters, dispatch, render, map errors to
//! an [`ExitCode`]. `main.rs` owns the single `process::exit`.
//!
//! The pieces live next door: [`mod@dispatch`] knows which commands exist,
//! [`reporting`] owns everything written to stderr, [`wiring`] owns the choice
//! of adapters, and [`output`] owns every rendering.

pub mod args;
pub mod cli;
pub mod commands;
pub mod dispatch;
pub mod donate;
pub mod donate_display;
pub mod env;
pub mod host;
pub mod output;
pub mod reporting;
pub mod tty_prompter;
pub mod wiring;

use std::ffi::OsString;
use std::sync::Arc;

use broza::config::{CliOverrides, Config, EnvSnapshot};
use broza::model::Warning;
use broza::units::{ByteSize, DurationSpec};
use broza::{BrozaError, ExitCode};
use clap::Parser;

use crate::args::split_categories;
use crate::cli::{Cli, Command, GlobalArgs};
use crate::dispatch::{Inputs, dispatch};
use crate::env::RuntimeEnv;
use crate::output::{ColorPolicy, OutputFormat, Sink};
use crate::reporting::{report_clap_error, report_error, report_warnings};

/// Parse `args` and run the requested command, returning the process exit code.
pub fn run<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    run_with_env(args, &RuntimeEnv::from_process())
}

/// [`run`] against an explicit environment snapshot.
///
/// Tests use this to run the whole pipeline without the real `$HOME`, the real
/// terminal or the real environment variables.
pub fn run_with_env<I, T>(args: I, runtime: &RuntimeEnv) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => return report_clap_error(&error),
    };
    match execute(&cli, runtime) {
        Ok(code) => code,
        Err(error) => report_error(&error, &cli.global),
    }
}

/// The pipeline proper, with every failure surfaced as a [`BrozaError`].
fn execute(cli: &Cli, runtime: &RuntimeEnv) -> Result<ExitCode, BrozaError> {
    cli.validate()?;
    let sink = Sink::resolve(cli.global.output.as_deref());
    let Some(command) = cli.command.as_ref() else {
        return print_short_help(&sink);
    };

    let core_env = runtime.to_core_snapshot();
    let (file, effective) = resolve_config(cli, &core_env)?;

    let ports = wiring::ports(runtime)?;
    let report = crate::host::provider(runtime.host_override.as_deref(), Arc::clone(&ports.process)).report();
    let warnings: Vec<Warning> = report.warning.into_iter().collect();
    let format = OutputFormat::resolve(cli.global.json, cli.global.csv);
    let policy = ColorPolicy::resolve(cli.global.no_color, effective.color, runtime, format);

    let donate_prompt = effective.donate_prompt;
    let outcome = dispatch(
        command,
        cli,
        &core_env,
        Inputs { runtime, ports, file, effective, host: report.host, warnings, format, policy },
    )?;
    sink.write(&outcome.rendered)?;
    report_warnings(&outcome.warnings, &cli.global, format);
    donate_display::maybe_show(
        &donate_display::Run {
            outcome: &outcome,
            global: &cli.global,
            runtime,
            donate_prompt,
            format,
            policy,
        },
        &mut std::io::stderr(),
    );
    Ok(outcome.code)
}

/// Load the file and apply profile, environment and flags on top of it.
///
/// Commands that create the file tolerate its absence; the rest treat a file
/// the user explicitly named but that does not exist as an error (exit `2`).
fn resolve_config(cli: &Cli, core_env: &EnvSnapshot) -> Result<(Config, Config), BrozaError> {
    let path = cli.global.config.as_deref();
    let file = if cli.command.as_ref().is_some_and(Command::creates_config_file) {
        broza::config::load_optional(path, core_env)?.unwrap_or_default()
    } else {
        broza::config::load(path, core_env)?
    };
    let effective = broza::config::layer(&file, cli.global.profile.as_deref(), core_env, &overrides(cli)?)?;
    Ok((file, effective))
}

/// Values the flags contribute to the configuration layering.
///
/// Only flags the user actually passed appear here: a clap `default_value`
/// would otherwise silently outrank `config.toml` and the active profile.
/// `scan --min-size` is deliberately absent — it has its own default
/// (`args::scan::SCAN_DEFAULT_MIN_SIZE`) and never feeds the `min-size` key.
///
/// # Errors
///
/// [`BrozaError::Usage`] when a size or duration flag does not parse; the
/// message comes straight from the unit parser in `broza::units`.
fn overrides(cli: &Cli) -> Result<CliOverrides, BrozaError> {
    let GlobalArgs { no_color, .. } = cli.global;
    let base = CliOverrides::default().with_no_color(no_color);
    let merged = match cli.command.as_ref() {
        Some(Command::Suggest(args)) => base
            .with_unused_after(parse_opt::<DurationSpec>(args.unused_after.as_deref())?)
            .with_min_size(parse_opt::<ByteSize>(args.min_size.as_deref())?)
            .with_categories(non_empty(split_categories(&args.categories))),
        Some(Command::Clean(args)) => base
            .with_unused_after(parse_opt::<DurationSpec>(args.unused_after.as_deref())?)
            .with_exclude(args.exclude.clone())
            .with_categories(non_empty(split_categories(&args.categories))),
        _ => base,
    };
    Ok(merged)
}

/// Parse a flag value that may be absent, keeping the parser's own message.
fn parse_opt<T: std::str::FromStr<Err = BrozaError>>(raw: Option<&str>) -> Result<Option<T>, BrozaError> {
    raw.map(str::parse).transpose()
}

fn non_empty(values: Vec<String>) -> Option<Vec<String>> {
    (!values.is_empty()).then_some(values)
}

/// Bare `broza`: short help through the normal sink, exit `0` (`docs/cli-spec.md` §1).
fn print_short_help(sink: &Sink) -> Result<ExitCode, BrozaError> {
    use clap::CommandFactory;

    sink.write(&Cli::command().render_help().to_string())?;
    Ok(ExitCode::Ok)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn suggest_flags_become_configuration_overrides() {
        let cli = parse(&["broza", "suggest", "--min-size", "2GB", "--category", "a,b"]);
        let overrides = overrides(&cli).unwrap();
        assert_eq!(overrides.min_size.map(ByteSize::bytes), Some(2_000_000_000));
        assert_eq!(overrides.categories, Some(vec!["a".to_owned(), "b".to_owned()]));
        assert!(!overrides.no_color);
    }

    #[test]
    fn absent_flags_leave_the_configuration_alone() {
        let overrides = overrides(&parse(&["broza", "suggest"])).unwrap();
        assert_eq!(overrides.min_size, None, "no flag must not shadow config.toml");
        assert_eq!(overrides.unused_after, None);
        assert_eq!(overrides.categories, None);
    }

    #[test]
    fn scan_min_size_never_feeds_the_configuration_key() {
        let overrides = overrides(&parse(&["broza", "scan", "--min-size", "1GB"])).unwrap();
        assert_eq!(overrides.min_size, None, "scan has its own default, see spec §3.1");
    }

    #[test]
    fn clean_exclusions_reach_the_layering() {
        let cli = parse(&["broza", "clean", "--exclude", "~/a/**", "--no-color"]);
        let overrides = overrides(&cli).unwrap();
        assert_eq!(overrides.exclude, vec!["~/a/**".to_owned()]);
        assert!(overrides.no_color);
    }

    #[test]
    fn commands_without_tunable_flags_only_carry_no_color() {
        let overrides = overrides(&parse(&["broza", "about", "--no-color"])).unwrap();
        assert_eq!(overrides, CliOverrides::default().with_no_color(true));
    }

    /// The regression the review asked for: a profile value must survive when
    /// the corresponding flag is absent.
    #[test]
    fn a_profile_value_survives_when_the_flag_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "[profiles.developer]\nmin-size = \"500MB\"\n").unwrap();
        let core_env = EnvSnapshot::for_home(dir.path().to_path_buf());

        let args = ["broza", "suggest", "--profile", "developer", "--config", &config.display().to_string()]
            .map(ToString::to_string);
        let cli = Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{e}"));
        let (_, effective) = resolve_config(&cli, &core_env).unwrap();
        assert_eq!(effective.min_size.to_string(), "500MB");
    }

    #[test]
    fn an_explicit_flag_still_outranks_the_profile() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "[profiles.developer]\nmin-size = \"500MB\"\n").unwrap();
        let core_env = EnvSnapshot::for_home(dir.path().to_path_buf());

        let args = [
            "broza",
            "suggest",
            "--profile",
            "developer",
            "--min-size",
            "2GB",
            "--config",
            &config.display().to_string(),
        ]
        .map(ToString::to_string);
        let cli = Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{e}"));
        let (_, effective) = resolve_config(&cli, &core_env).unwrap();
        assert_eq!(effective.min_size.to_string(), "2GB");
    }
}
