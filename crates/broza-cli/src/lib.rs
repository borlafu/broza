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

use std::ffi::OsString;
use std::io::Write;

use broza::config::{CliOverrides, Config};
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
        Err(error) => report_error(&error),
    }
}

/// The pipeline proper, with every failure surfaced as a [`BrozaError`].
fn execute(cli: &Cli, runtime: &RuntimeEnv) -> Result<ExitCode, BrozaError> {
    cli.validate()?;
    let Some(command) = cli.command.as_ref() else {
        return print_short_help();
    };

    let core_env = runtime.to_core_snapshot();
    let file = broza::config::load(cli.global.config.as_deref(), &core_env)?;
    let effective = broza::config::layer(&file, cli.global.profile.as_deref(), &core_env, &overrides(cli))?;

    let rendered = dispatch(command, cli, runtime, &file, &effective)?;
    Sink::resolve(cli.global.output.as_deref()).write(&rendered)?;
    Ok(ExitCode::Ok)
}

/// Run one command and render it in the selected format.
fn dispatch(
    command: &Command,
    cli: &Cli,
    runtime: &RuntimeEnv,
    file: &Config,
    effective: &Config,
) -> Result<String, BrozaError> {
    let format = OutputFormat::resolve(cli.global.json, cli.global.csv);
    let host = crate::host::provider(runtime.host_override.as_deref()).host();
    let generated_at = jiff::Timestamp::now().to_string();

    match command {
        Command::About => commands::about::About::new(host, generated_at).render(format),
        Command::Config(args) => {
            let context = ConfigContext {
                path: broza::config::resolve_path(cli.global.config.as_deref(), &runtime.to_core_snapshot()),
                file: file.clone(),
                effective: effective.clone(),
                host,
                generated_at,
                interactive: runtime.is_interactive(),
            };
            commands::config::run(&args.command, &context)?.render(format)
        }
        other => Err(commands::not_implemented(other.name())),
    }
}

/// Values the flags contribute to the configuration layering.
fn overrides(cli: &Cli) -> CliOverrides {
    let GlobalArgs { no_color, .. } = cli.global;
    match cli.command.as_ref() {
        Some(Command::Suggest(args)) => CliOverrides {
            unused_after: Some(args.unused_after.clone()),
            min_size: Some(args.min_size.clone()),
            no_color,
            exclude: Vec::new(),
            categories: non_empty(split_categories(&args.categories)),
        },
        Some(Command::Clean(args)) => CliOverrides {
            unused_after: Some(args.unused_after.clone()),
            min_size: None,
            no_color,
            exclude: args.exclude.clone(),
            categories: non_empty(split_categories(&args.categories)),
        },
        Some(Command::Scan(args)) => {
            CliOverrides { min_size: Some(args.min_size.clone()), no_color, ..CliOverrides::default() }
        }
        _ => CliOverrides { no_color, ..CliOverrides::default() },
    }
}

fn non_empty(values: Vec<String>) -> Option<Vec<String>> {
    (!values.is_empty()).then_some(values)
}

/// Bare `broza`: short help on stdout, exit `0` (`docs/cli-spec.md` §1).
fn print_short_help() -> Result<ExitCode, BrozaError> {
    use clap::CommandFactory;

    let help = Cli::command().render_help().to_string();
    std::io::stdout()
        .write_all(help.as_bytes())
        .map_err(|source| BrozaError::Io { context: "writing help".to_owned(), source })?;
    Ok(ExitCode::Ok)
}

/// `--help` and `--version` are successes; every other clap error is exit `2`.
fn report_clap_error(error: &clap::Error) -> ExitCode {
    let is_help_or_version = matches!(error.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion);
    let _ignored = error.print();
    if is_help_or_version { ExitCode::Ok } else { ExitCode::UsageError }
}

/// Report `error` on stderr (principle 5) and map it to its exit code.
fn report_error(error: &BrozaError) -> ExitCode {
    let _ignored = writeln!(std::io::stderr(), "error: {error}");
    ExitCode::from(error)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{e}"))
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
        assert_eq!(report_error(&BrozaError::Usage("x".into())), ExitCode::UsageError);
        assert_eq!(report_error(&BrozaError::ConfirmationRequired), ExitCode::ConfirmationRequired);
        assert_eq!(report_error(&BrozaError::Other("x".into())), ExitCode::GenericError);
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
    fn clean_exclusions_reach_the_layering() {
        let cli = parse(&["broza", "clean", "--exclude", "~/a/**", "--no-color"]);
        let overrides = overrides(&cli);
        assert_eq!(overrides.exclude, vec!["~/a/**".to_owned()]);
        assert!(overrides.no_color);
    }

    #[test]
    fn commands_without_tunable_flags_only_carry_no_color() {
        let overrides = overrides(&parse(&["broza", "about", "--no-color"]));
        assert_eq!(overrides, CliOverrides { no_color: true, ..CliOverrides::default() });
    }

    #[test]
    fn scan_min_size_reaches_the_layering() {
        let overrides = overrides(&parse(&["broza", "scan", "--min-size", "1GB"]));
        assert_eq!(overrides.min_size, Some("1GB".to_owned()));
    }

    #[test]
    fn empty_category_lists_do_not_override_the_profile() {
        assert_eq!(overrides(&parse(&["broza", "suggest"])).categories, None);
    }

    /// A pipeline run rooted at a temporary home, writing its data to a file so
    /// nothing leaks into the test harness's stdout.
    fn run_in(home: &std::path::Path, args: &[&str]) -> (ExitCode, String) {
        let target = home.join("captured-output");
        let runtime = RuntimeEnv {
            host_override: Some("26.1/arm64".to_owned()),
            ..RuntimeEnv::for_tests(home.to_path_buf())
        };
        let mut full: Vec<String> = args.iter().map(ToString::to_string).collect();
        full.push("--output".to_owned());
        full.push(target.display().to_string());
        let code = run_with_env(full, &runtime);
        (code, std::fs::read_to_string(&target).unwrap_or_default())
    }

    fn temp_home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn about_runs_end_to_end() {
        let home = temp_home();
        let (code, text) = run_in(home.path(), &["broza", "about"]);
        assert_eq!(code, ExitCode::Ok);
        assert!(text.contains("ko-fi.com/broza"), "{text}");
    }

    #[test]
    fn about_json_carries_the_injected_host() {
        let home = temp_home();
        let (code, text) = run_in(home.path(), &["broza", "about", "--json"]);
        assert_eq!(code, ExitCode::Ok);
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parsed["host"]["macos_version"], "26.1");
        assert_eq!(parsed["schema_version"], broza::SCHEMA_VERSION);
    }

    #[test]
    fn config_runs_against_the_temporary_home() {
        let home = temp_home();
        assert_eq!(run_in(home.path(), &["broza", "config", "set", "min-size", "2GB"]).0, ExitCode::Ok);
        let (code, text) = run_in(home.path(), &["broza", "config", "get", "min-size"]);
        assert_eq!(code, ExitCode::Ok);
        assert_eq!(text.trim(), "2GB");
    }

    #[test]
    fn rejected_flag_combinations_never_reach_a_command() {
        let home = temp_home();
        assert_eq!(run_in(home.path(), &["broza", "about", "--json", "--csv"]).0, ExitCode::UsageError);
        assert_eq!(run_in(home.path(), &["broza", "about", "--csv"]).0, ExitCode::UsageError);
        assert_eq!(run_in(home.path(), &["broza", "clean", "--purge", "--yes"]).0, ExitCode::UsageError);
    }

    #[test]
    fn pending_commands_exit_one() {
        let home = temp_home();
        for command in ["scan", "suggest", "clean"] {
            assert_eq!(run_in(home.path(), &["broza", command]).0, ExitCode::GenericError, "{command}");
        }
        assert_eq!(run_in(home.path(), &["broza", "explain", "snapshots"]).0, ExitCode::GenericError);
        assert_eq!(run_in(home.path(), &["broza", "restore", "--all"]).0, ExitCode::GenericError);
        assert_eq!(run_in(home.path(), &["broza", "quarantine", "list"]).0, ExitCode::GenericError);
    }

    #[test]
    fn an_invalid_configuration_file_stops_the_pipeline() {
        let home = temp_home();
        let config = home.path().join(".config/broza/config.toml");
        std::fs::create_dir_all(config.parent().unwrap_or_else(|| panic!("parent"))).unwrap();
        std::fs::write(&config, "unused-aftr = \"1y\"\n").unwrap();
        assert_eq!(run_in(home.path(), &["broza", "config", "list"]).0, ExitCode::UsageError);
    }

    #[test]
    fn an_unwritable_output_path_is_a_generic_error() {
        let runtime = RuntimeEnv::for_tests(std::path::PathBuf::from("/Users/test"));
        let code = run_with_env(["broza", "about", "--output", "/nonexistent-broza-dir/out.txt"], &runtime);
        assert_eq!(code, ExitCode::GenericError);
    }

    #[test]
    fn unparseable_arguments_never_reach_the_pipeline() {
        let runtime = RuntimeEnv::for_tests(std::path::PathBuf::from("/Users/test"));
        assert_eq!(run_with_env(["broza", "--nope"], &runtime), ExitCode::UsageError);
    }

    #[test]
    fn run_uses_the_real_environment_without_panicking() {
        assert_eq!(run(["broza", "--version"]), ExitCode::Ok);
    }

    #[test]
    fn a_bare_invocation_prints_the_short_help_and_succeeds() {
        let runtime = RuntimeEnv::for_tests(std::path::PathBuf::from("/Users/test"));
        assert_eq!(run_with_env(["broza"], &runtime), ExitCode::Ok);
    }
}
