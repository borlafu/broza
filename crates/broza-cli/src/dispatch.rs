//! Routing one parsed command to its implementation.
//!
//! The only place that knows which commands exist. [`Inputs`] carries what
//! every command needs and nothing it does not: each arm picks its own fields
//! out of it, so adding a command touches this file and that command's module,
//! and nothing in the pipeline.

use broza::BrozaError;
use broza::config::{Config, EnvSnapshot};
use broza::model::{Host, Warning};
use broza::ports::Ports;
use std::path::PathBuf;

use crate::cli::{Cli, Command};
use crate::commands::clean::CleanContext;
use crate::commands::config::ConfigContext;
use crate::commands::explain::ExplainContext;
use crate::commands::scan::ScanContext;
use crate::commands::scan::folders::FolderSettings;
use crate::commands::store::StoreContext;
use crate::commands::suggest::SuggestContext;
use crate::commands::{self, Outcome};
use crate::env::RuntimeEnv;
use crate::output::{ColorPolicy, OutputFormat, Renderer};

/// Everything [`dispatch`] needs beyond the command itself.
pub struct Inputs<'a> {
    /// The environment snapshot the run was started with.
    pub runtime: &'a RuntimeEnv,
    /// The wired adapters.
    pub ports: Ports,
    /// The configuration file as written, before layering.
    pub file: Config,
    /// The configuration after profile, environment and flags.
    pub effective: Config,
    /// `host` block of the envelope.
    pub host: Host,
    /// Warnings raised before any command ran.
    pub warnings: Vec<Warning>,
    /// Format the caller asked for.
    pub format: OutputFormat,
    /// Whether the human rendering may use colour.
    pub policy: ColorPolicy,
}

/// Run one command and render it in the selected format.
///
/// # Errors
///
/// Whatever the command returns, plus [`BrozaError::Other`] for the commands
/// whose engine does not exist yet.
pub fn dispatch(
    command: &Command,
    cli: &Cli,
    core_env: &EnvSnapshot,
    inputs: Inputs<'_>,
) -> Result<Outcome, BrozaError> {
    let generated_at = jiff::Timestamp::now();
    let folders = folder_settings(cli, &inputs);
    match command {
        Command::About => {
            let rendered = commands::about::About::new(inputs.host, generated_at, inputs.warnings.clone())
                .render(inputs.format)?;
            Ok(Outcome::ok(rendered).with_warnings(inputs.warnings))
        }
        Command::Config(args) => {
            let context = ConfigContext {
                path: broza::config::resolve_path(cli.global.config.as_deref(), core_env)?,
                file: inputs.file,
                effective: inputs.effective,
                host: inputs.host,
                generated_at,
                interactive: inputs.runtime.is_interactive(),
                warnings: inputs.warnings.clone(),
            };
            let rendered = commands::config::run(&args.command, &context)?.render(inputs.format)?;
            Ok(Outcome::ok(rendered).with_warnings(inputs.warnings))
        }
        Command::Scan(args) => commands::scan::run(&ScanContext {
            ports: &inputs.ports,
            args,
            host: inputs.host,
            generated_at,
            warnings: inputs.warnings,
            policy: inputs.policy,
            format: inputs.format,
            folders: folders.clone(),
        }),
        Command::Suggest(args) => commands::suggest::run(&SuggestContext {
            ports: &inputs.ports,
            args,
            host: inputs.host,
            generated_at,
            warnings: inputs.warnings,
            policy: inputs.policy,
            format: inputs.format,
            folders: folders.clone(),
            config_min_size: inputs.effective.min_size,
            config_unused_after: inputs.effective.unused_after,
        }),
        Command::Clean(args) => commands::clean::run(&CleanContext {
            ports: &inputs.ports,
            args,
            config: &inputs.effective,
            host: inputs.host,
            generated_at,
            warnings: inputs.warnings,
            policy: inputs.policy,
            format: inputs.format,
            folders: folders.clone(),
            tty: inputs.runtime.is_interactive(),
            ci: inputs.runtime.ci,
            uid_temp_dirs: inputs.runtime.uid_temp_dirs(),
        }),
        Command::Restore(args) => commands::restore::run(args, &store_context(&inputs, generated_at)),
        Command::Quarantine(args) => {
            commands::quarantine::run(&args.command, &store_context(&inputs, generated_at))
        }
        Command::Explain(args) => commands::explain::run(&ExplainContext {
            ports: &inputs.ports,
            args,
            cwd: inputs.runtime.cwd.as_deref(),
            host: inputs.host,
            generated_at,
            warnings: inputs.warnings,
            policy: inputs.policy,
            format: inputs.format,
        }),
    }
}

/// What the store commands (`restore`, `quarantine`) need from the run.
fn store_context<'a>(inputs: &'a Inputs<'a>, generated_at: jiff::Timestamp) -> StoreContext<'a> {
    StoreContext {
        ports: &inputs.ports,
        config: &inputs.effective,
        host: inputs.host.clone(),
        generated_at,
        warnings: inputs.warnings.clone(),
        format: inputs.format,
        home: walked_home(inputs.runtime),
        uid_temp_dirs: inputs.runtime.uid_temp_dirs(),
    }
}

/// The home a folder walk starts from: the real one, or the recording's under
/// the debug-only fixture seam, where the real one does not exist.
fn walked_home(runtime: &RuntimeEnv) -> Option<PathBuf> {
    crate::wiring::fixture_home(runtime).or_else(|| runtime.home.clone())
}

/// Home, cache and progress settings shared by every command that walks folders.
fn folder_settings(cli: &Cli, inputs: &Inputs<'_>) -> FolderSettings {
    // Both the home and the stores excluded under it come from the same
    // spelling: an exclusion for a home that is not walked excludes nothing.
    let home = walked_home(inputs.runtime);
    FolderSettings {
        own_stores: home
            .as_deref()
            .map(|home| vec![home.join(".cache/broza"), inputs.effective.quarantine_dir(home)])
            .unwrap_or_default(),
        home,
        cache_ttl: inputs.effective.cache_ttl.to_duration(),
        no_cache: cli.global.no_cache,
        show_progress: inputs.runtime.stderr_is_tty
            && !inputs.runtime.ci
            && !cli.global.quiet
            && inputs.format == OutputFormat::Human,
        verbose: cli.global.verbose > 0,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;

    use broza::ExitCode;
    use clap::Parser;

    use super::*;

    fn inputs(ports: Ports) -> Inputs<'static> {
        // The runtime outlives the call; leaking one snapshot keeps the test
        // free of a lifetime dance that proves nothing.
        let runtime: &'static RuntimeEnv =
            Box::leak(Box::new(RuntimeEnv::for_tests(PathBuf::from("/Users/test"))));
        Inputs {
            runtime,
            ports,
            file: Config::default(),
            effective: Config::default(),
            host: Host { macos_version: "26.1".into(), arch: "arm64".into() },
            warnings: Vec::new(),
            format: OutputFormat::Human,
            policy: ColorPolicy::Never,
        }
    }

    fn route(argv: &[&str]) -> Result<Outcome, BrozaError> {
        let cli = Cli::try_parse_from(argv).unwrap_or_else(|e| panic!("{e}"));
        let command = cli.command.clone().unwrap_or_else(|| panic!("a command"));
        let (ports, _handles) = broza::testing::fake_ports();
        dispatch(&command, &cli, &EnvSnapshot::default(), inputs(ports))
    }

    #[test]
    fn about_is_routed_and_rendered() {
        let outcome = route(&["broza", "about"]).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(outcome.code, ExitCode::Ok);
        assert!(outcome.rendered.contains("Broza"), "{}", outcome.rendered);
    }

    #[test]
    fn explain_is_routed_to_its_command() {
        let outcome = route(&["broza", "explain", "user-cache"]).unwrap_or_else(|e| panic!("{e}"));

        assert!(outcome.rendered.starts_with("user-cache"), "{}", outcome.rendered);
    }

    #[test]
    fn scan_is_routed_and_answers_from_the_ports_it_was_given() {
        let outcome = route(&["broza", "scan"]).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(outcome.rendered, "No disks found.", "the fake enumerator knows no disks");
    }

    #[test]
    fn config_is_routed_with_the_path_it_resolves_for_this_home() {
        let home = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let core_env = EnvSnapshot::for_home(home.path().to_path_buf());
        let cli = Cli::try_parse_from(["broza", "config", "list"]).unwrap_or_else(|e| panic!("{e}"));
        let command = cli.command.clone().unwrap_or_else(|| panic!("a command"));
        let (ports, _handles) = broza::testing::fake_ports();

        let outcome = dispatch(&command, &cli, &core_env, inputs(ports)).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(outcome.code, ExitCode::Ok);
        assert!(outcome.rendered.contains("unused-after"), "{}", outcome.rendered);
    }

    #[test]
    fn a_config_path_that_cannot_be_resolved_fails_before_the_command_runs() {
        let cli = Cli::try_parse_from(["broza", "config", "list"]).unwrap_or_else(|e| panic!("{e}"));
        let command = cli.command.clone().unwrap_or_else(|| panic!("a command"));
        let (ports, _handles) = broza::testing::fake_ports();

        // No `--config` and no home: there is nowhere the file could be.
        let error = dispatch(&command, &cli, &EnvSnapshot::default(), inputs(ports)).expect_err("must fail");

        assert_eq!(ExitCode::from(&error), ExitCode::UsageError);
    }

    #[test]
    fn warnings_raised_before_the_command_survive_the_routing() {
        let cli = Cli::try_parse_from(["broza", "about"]).unwrap_or_else(|e| panic!("{e}"));
        let command = cli.command.clone().unwrap_or_else(|| panic!("a command"));
        let (ports, _handles) = broza::testing::fake_ports();
        let warning = Warning { code: "host_version_unknown".into(), message: "m".into(), path: None };
        let inputs = Inputs { warnings: vec![warning.clone()], ..inputs(ports) };

        let outcome =
            dispatch(&command, &cli, &EnvSnapshot::default(), inputs).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(outcome.warnings, vec![warning]);
    }
}
