//! Broza command-line interface: argument parsing, rendering, TTY prompting.
//!
//! [`run`] is the whole pipeline — parse, snapshot the environment, load and
//! layer the configuration, dispatch, render, map errors to an [`ExitCode`].
//! `main.rs` owns the single `process::exit`.

pub mod args;
pub mod cli;
pub mod commands;
pub mod donate;
pub mod env;
pub mod host;
pub mod output;

use std::error::Error as _;
use std::ffi::OsString;

use broza::config::{CliOverrides, Config, EnvSnapshot};
use broza::model::Warning;
use broza::{BrozaError, ExitCode};
use clap::Parser;
use clap::error::ErrorKind;

use crate::args::split_categories;
use crate::cli::{Cli, Command, GlobalArgs};
use crate::commands::config::ConfigContext;
use crate::env::RuntimeEnv;
use crate::output::{OutputFormat, Renderer, Sink};

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

    let report = crate::host::provider(runtime.host_override.as_deref()).report();
    let warnings: Vec<Warning> = report.warning.into_iter().collect();
    let format = OutputFormat::resolve(cli.global.json, cli.global.csv);

    let rendered = dispatch(
        command,
        cli,
        &core_env,
        Inputs { runtime, file, effective, host: report.host, warnings: warnings.clone(), format },
    )?;
    sink.write(&rendered)?;
    report_warnings(&warnings, &cli.global, format);
    Ok(ExitCode::Ok)
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
    let effective = broza::config::layer(&file, cli.global.profile.as_deref(), core_env, &overrides(cli))?;
    Ok((file, effective))
}

/// Everything `dispatch` needs beyond the command itself.
struct Inputs<'a> {
    runtime: &'a RuntimeEnv,
    file: Config,
    effective: Config,
    host: broza::model::Host,
    warnings: Vec<Warning>,
    format: OutputFormat,
}

/// Run one command and render it in the selected format.
fn dispatch(
    command: &Command,
    cli: &Cli,
    core_env: &EnvSnapshot,
    inputs: Inputs<'_>,
) -> Result<String, BrozaError> {
    let generated_at = now_rfc3339();
    match command {
        Command::About => {
            commands::about::About::new(inputs.host, generated_at, inputs.warnings).render(inputs.format)
        }
        Command::Config(args) => {
            let context = ConfigContext {
                path: broza::config::resolve_path(cli.global.config.as_deref(), core_env)?,
                file: inputs.file,
                effective: inputs.effective,
                host: inputs.host,
                generated_at,
                interactive: inputs.runtime.is_interactive(),
                warnings: inputs.warnings,
            };
            commands::config::run(&args.command, &context)?.render(inputs.format)
        }
        other => Err(commands::not_implemented(other.name())),
    }
}

/// Current time as an RFC 3339 UTC timestamp with whole-second precision
/// (`docs/cli-spec.md` §4.1): sub-second digits would only churn snapshots.
pub fn now_rfc3339() -> String {
    let now = jiff::Timestamp::now();
    now.round(jiff::Unit::Second).unwrap_or(now).to_string()
}

/// Values the flags contribute to the configuration layering.
///
/// Only flags the user actually passed appear here: a clap `default_value`
/// would otherwise silently outrank `config.toml` and the active profile.
/// `scan --min-size` is deliberately absent — it has its own default
/// (`args::scan::SCAN_DEFAULT_MIN_SIZE`) and never feeds the `min-size` key.
fn overrides(cli: &Cli) -> CliOverrides {
    let GlobalArgs { no_color, .. } = cli.global;
    let base = CliOverrides::default().with_no_color(no_color);
    match cli.command.as_ref() {
        Some(Command::Suggest(args)) => base
            .with_unused_after(args.unused_after.clone())
            .with_min_size(args.min_size.clone())
            .with_categories(non_empty(split_categories(&args.categories))),
        Some(Command::Clean(args)) => base
            .with_unused_after(args.unused_after.clone())
            .with_exclude(args.exclude.clone())
            .with_categories(non_empty(split_categories(&args.categories))),
        _ => base,
    }
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

/// Warnings are conversation, so they go to stderr and `--quiet` silences them.
/// In JSON and CSV runs they travel inside the envelope instead.
fn report_warnings(warnings: &[Warning], global: &GlobalArgs, format: OutputFormat) {
    use std::io::Write;

    if global.quiet || format.is_machine_readable() {
        return;
    }
    for warning in warnings {
        let _ignored = writeln!(std::io::stderr(), "warning: {}", warning.message);
    }
}

/// `--help` and `--version` are successes; every other clap error is exit `2`.
fn report_clap_error(error: &clap::Error) -> ExitCode {
    let is_help_or_version = matches!(error.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion);
    let _ignored = error.print();
    if is_help_or_version { ExitCode::Ok } else { ExitCode::UsageError }
}

/// Report `error` on stderr (principle 5) and map it to its exit code.
///
/// Errors are never silenced by `--quiet`; `-v` adds the source chain.
fn report_error(error: &BrozaError, global: &GlobalArgs) -> ExitCode {
    use std::io::Write;

    let mut stderr = std::io::stderr();
    let _ignored = writeln!(stderr, "error: {error}");
    if global.verbose > 0 {
        let mut source = error.source();
        while let Some(cause) = source {
            let _ignored = writeln!(stderr, "  caused by: {cause}");
            source = cause.source();
        }
    }
    ExitCode::from(error)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{e}"))
    }

    fn quiet() -> GlobalArgs {
        GlobalArgs::default()
    }

    #[test]
    fn help_and_version_are_successes() {
        let help = Cli::try_parse_from(["broza", "--help"]).expect_err("clap returns an error");
        assert_eq!(report_clap_error(&help), ExitCode::Ok);
        let version = Cli::try_parse_from(["broza", "--version"]).expect_err("clap returns an error");
        assert_eq!(report_clap_error(&version), ExitCode::Ok);
    }

    #[test]
    fn unknown_flags_are_usage_errors() {
        let error = Cli::try_parse_from(["broza", "--nope"]).expect_err("must fail");
        assert_eq!(report_clap_error(&error), ExitCode::UsageError);
    }

    #[test]
    fn core_errors_keep_their_exit_code() {
        assert_eq!(report_error(&BrozaError::Usage("x".into()), &quiet()), ExitCode::UsageError);
        assert_eq!(report_error(&BrozaError::ConfirmationRequired, &quiet()), ExitCode::ConfirmationRequired);
        assert_eq!(report_error(&BrozaError::Other("x".into()), &quiet()), ExitCode::GenericError);
    }

    #[test]
    fn verbose_does_not_change_the_exit_code_of_a_sourced_error() {
        let error = BrozaError::Io {
            context: "writing".to_owned(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        };
        let verbose = GlobalArgs { verbose: 2, ..GlobalArgs::default() };
        assert_eq!(report_error(&error, &verbose), ExitCode::GenericError);
        assert_eq!(report_error(&error, &quiet()), ExitCode::GenericError);
    }

    #[test]
    fn quiet_silences_warnings_but_never_errors() {
        let warnings = vec![Warning { code: "c".into(), message: "m".into(), path: None }];
        let loud = GlobalArgs::default();
        let hushed = GlobalArgs { quiet: true, ..GlobalArgs::default() };
        // Exercises both branches; the assertion is that neither panics nor
        // changes state, the visible effect being stderr only.
        report_warnings(&warnings, &loud, OutputFormat::Human);
        report_warnings(&warnings, &hushed, OutputFormat::Human);
        report_warnings(&warnings, &loud, OutputFormat::Json);
        assert_eq!(report_error(&BrozaError::Usage("x".into()), &hushed), ExitCode::UsageError);
    }

    #[test]
    fn timestamps_have_whole_second_precision() {
        let now = now_rfc3339();
        assert!(now.ends_with('Z'), "{now}");
        assert!(!now.contains('.'), "sub-second digits must be dropped: {now}");
    }

    #[test]
    fn suggest_flags_become_configuration_overrides() {
        let cli = parse(&["broza", "suggest", "--min-size", "2GB", "--category", "a,b"]);
        let overrides = overrides(&cli);
        assert_eq!(overrides.min_size, Some("2GB".to_owned()));
        assert_eq!(overrides.categories, Some(vec!["a".to_owned(), "b".to_owned()]));
        assert!(!overrides.no_color);
    }

    #[test]
    fn absent_flags_leave_the_configuration_alone() {
        let overrides = overrides(&parse(&["broza", "suggest"]));
        assert_eq!(overrides.min_size, None, "no flag must not shadow config.toml");
        assert_eq!(overrides.unused_after, None);
        assert_eq!(overrides.categories, None);
    }

    #[test]
    fn scan_min_size_never_feeds_the_configuration_key() {
        let overrides = overrides(&parse(&["broza", "scan", "--min-size", "1GB"]));
        assert_eq!(overrides.min_size, None, "scan has its own default, see spec §3.1");
    }

    #[test]
    fn clean_exclusions_reach_the_layering() {
        let cli = parse(&["broza", "clean", "--exclude", "~/a/**", "--no-color"]);
        let overrides = overrides(&cli);
        assert_eq!(overrides.exclude, vec!["~/a/**".to_owned()]);
        assert!(overrides.no_color);
    }

    #[test]
    fn commands_without_tunable_flags_only_carry_no_color() {
        let overrides = overrides(&parse(&["broza", "about", "--no-color"]));
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
        assert_eq!(effective.min_size, "500MB");
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
        assert_eq!(effective.min_size, "2GB");
    }
}
