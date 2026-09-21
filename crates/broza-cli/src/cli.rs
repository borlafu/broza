//! Top-level clap tree and the flag-combination rules of `docs/cli-spec.md` §1.1 and §2.

use std::path::PathBuf;
use std::sync::LazyLock;

use broza::BrozaError;
use clap::{ArgAction, Args, Parser, Subcommand};

use crate::args::{
    CleanArgs, ConfigArgs, ConfigCommand, ExplainArgs, QuarantineArgs, QuarantineCommand, RestoreArgs,
    ScanArgs, SuggestArgs,
};

/// `-V/--version` text: binary version plus the JSON schema version, both read
/// from the core so the two can never drift apart.
pub static VERSION: LazyLock<String> =
    LazyLock::new(|| format!("{} (JSON schema {})", broza::BROZA_VERSION, broza::SCHEMA_VERSION));

/// Safe, explainable disk analysis and cleanup for macOS.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "broza",
    version = VERSION.as_str(),
    about = "Safe, explainable disk analysis and cleanup for macOS",
    long_about = None,
    disable_help_subcommand = true,
    arg_required_else_help = false
)]
pub struct Cli {
    /// Command to run. Without one, `broza` prints this help and exits 0.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Flags accepted by every command.
    #[command(flatten)]
    pub global: GlobalArgs,
}

/// Flags available on every command (`docs/cli-spec.md` §1.1).
// One field per documented flag: the specification decides the shape, not clippy.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, Args)]
pub struct GlobalArgs {
    /// JSON output on stdout. Disables color, progress and interactivity.
    #[arg(long, global = true)]
    pub json: bool,

    /// Flat CSV output on stdout. Only on tabular commands.
    #[arg(long, global = true)]
    pub csv: bool,

    /// Write output to a file instead of stdout.
    #[arg(short = 'o', long, global = true, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Disable ANSI color. Automatic without a TTY or with `NO_COLOR` set.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Errors only. Silences progress and non-critical warnings.
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,

    /// More diagnostic detail on stderr. Repeatable.
    #[arg(short = 'v', long, global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Alternative configuration file [default: ~/.config/broza/config.toml].
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Configuration profile to apply.
    #[arg(long, global = true, value_name = "NAME")]
    pub profile: Option<String>,

    /// Ignore the scan cache and force a full analysis.
    #[arg(long, global = true)]
    pub no_cache: bool,
}

/// Every `broza` subcommand.
#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Analyse storage and show disks, containers, volumes and real usage.
    Scan(ScanArgs),
    /// Explain what a volume, path or cleanup category is.
    Explain(ExplainArgs),
    /// Detect cleanable categories and estimate reclaimable space.
    Suggest(SuggestArgs),
    /// Execute the cleanup. Dry run by default.
    Clean(CleanArgs),
    /// Recover items from quarantine.
    Restore(RestoreArgs),
    /// Read and write configuration keys.
    Config(ConfigArgs),
    /// Manage the quarantine store.
    Quarantine(QuarantineArgs),
    /// Version, license, schema version and support link.
    About,
}

impl Command {
    /// Command name as it appears in the JSON envelope.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Scan(_) => "scan",
            Self::Explain(_) => "explain",
            Self::Suggest(_) => "suggest",
            Self::Clean(_) => "clean",
            Self::Restore(_) => "restore",
            Self::Config(_) => "config",
            Self::Quarantine(_) => "quarantine",
            Self::About => "about",
        }
    }

    /// Whether an explicitly named configuration file may be absent.
    ///
    /// `config set`, `config reset` and `config path` create or merely name the
    /// file, so `--config <new file>` is legitimate. Every other command reads
    /// it, and a file the user named but that does not exist is an error.
    pub const fn creates_config_file(&self) -> bool {
        match self {
            Self::Config(args) => matches!(
                args.command,
                ConfigCommand::Set { .. } | ConfigCommand::Reset { .. } | ConfigCommand::Path
            ),
            _ => false,
        }
    }

    /// Whether `--csv` is accepted (`docs/cli-spec.md` §1.1).
    pub const fn supports_csv(&self) -> bool {
        match self {
            Self::Scan(_) | Self::Suggest(_) => true,
            Self::Restore(args) => args.list,
            Self::Quarantine(args) => matches!(args.command, QuarantineCommand::List),
            _ => false,
        }
    }
}

impl Cli {
    /// Reject the flag combinations that `docs/cli-spec.md` §2 maps to exit `2`.
    ///
    /// # Errors
    ///
    /// [`BrozaError::Usage`] for every rejected combination.
    pub fn validate(&self) -> Result<(), BrozaError> {
        if self.global.json && self.global.csv {
            return Err(BrozaError::Usage(
                "--json and --csv are mutually exclusive: choose one output format".to_owned(),
            ));
        }
        let Some(command) = self.command.as_ref() else {
            return Ok(());
        };
        if self.global.csv && !command.supports_csv() {
            return Err(BrozaError::Usage(format!(
                "--csv is not supported by `{}`: it is available on scan, suggest, \
                 quarantine list and restore --list",
                command.name()
            )));
        }
        validate_command(command)
    }
}

/// Irreversible deletion never accepts an implicit "yes" (AGENTS.md §2, invariant 2).
fn yes_with_purge() -> BrozaError {
    BrozaError::Usage(
        "--yes cannot be combined with --purge: irreversible deletion always requires typing PURGE"
            .to_owned(),
    )
}

/// Per-command rules that do not depend on the global flags.
fn validate_command(command: &Command) -> Result<(), BrozaError> {
    match command {
        Command::Clean(args) if args.purge && args.yes => Err(yes_with_purge()),
        Command::Restore(args)
            if !args.list && args.ids.is_empty() && !args.all && args.session.is_none() =>
        {
            Err(BrozaError::Usage(
                "restore needs at least one ID, --all or --session (or --list to only look)".to_owned(),
            ))
        }
        Command::Quarantine(args) => validate_quarantine(&args.command),
        _ => Ok(()),
    }
}

fn validate_quarantine(command: &QuarantineCommand) -> Result<(), BrozaError> {
    match command {
        QuarantineCommand::Purge { yes: true, .. } => Err(yes_with_purge()),
        QuarantineCommand::Purge { sessions, all, .. } if sessions.is_empty() && !all => {
            Err(BrozaError::Usage("quarantine purge needs at least one SESSION_ID or --all".to_owned()))
        }
        QuarantineCommand::Purge { sessions, all, .. } if !sessions.is_empty() && *all => {
            Err(BrozaError::Usage("quarantine purge takes either SESSION_IDs or --all, not both".to_owned()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use clap::CommandFactory;

    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap_or_else(|e| panic!("parse {args:?}: {e}"))
    }

    #[test]
    fn clap_tree_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn version_text_is_built_from_the_core_constants() {
        assert!(VERSION.contains(broza::SCHEMA_VERSION), "{} must mention the schema", *VERSION);
        assert!(VERSION.starts_with(broza::BROZA_VERSION), "{} must start with the version", *VERSION);
    }

    #[test]
    fn bare_invocation_parses_without_a_command() {
        assert!(parse(&["broza"]).command.is_none());
    }

    #[test]
    fn global_flags_work_before_and_after_the_subcommand() {
        assert!(parse(&["broza", "--json", "about"]).global.json);
        assert!(parse(&["broza", "about", "--json"]).global.json);
        assert_eq!(parse(&["broza", "scan", "-vvv"]).global.verbose, 3);
    }

    #[test]
    fn categories_accept_commas_and_repetition() {
        let cli = parse(&["broza", "suggest", "--category", "a,b", "--category", "c"]);
        let Some(Command::Suggest(args)) = cli.command else { panic!("expected suggest") };
        assert_eq!(args.categories, vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]);
    }

    #[test]
    fn json_and_csv_together_are_rejected() {
        let err = parse(&["broza", "scan", "--json", "--csv"]).validate().expect_err("must fail");
        assert!(matches!(err, BrozaError::Usage(_)), "{err}");
    }

    #[test]
    fn csv_is_only_accepted_on_tabular_commands() {
        assert!(parse(&["broza", "scan", "--csv"]).validate().is_ok());
        assert!(parse(&["broza", "suggest", "--csv"]).validate().is_ok());
        assert!(parse(&["broza", "quarantine", "list", "--csv"]).validate().is_ok());
        assert!(parse(&["broza", "restore", "--list", "--csv"]).validate().is_ok());

        assert!(parse(&["broza", "about", "--csv"]).validate().is_err());
        assert!(parse(&["broza", "clean", "--csv"]).validate().is_err());
        assert!(parse(&["broza", "explain", "x", "--csv"]).validate().is_err());
        assert!(parse(&["broza", "quarantine", "expire", "--csv"]).validate().is_err());
    }

    #[test]
    fn yes_with_purge_is_rejected() {
        let err = parse(&["broza", "clean", "--purge", "--yes"]).validate().expect_err("must fail");
        assert!(err.to_string().contains("PURGE"), "{err}");
        assert!(parse(&["broza", "clean", "--purge"]).validate().is_ok());
        assert!(parse(&["broza", "clean", "--yes"]).validate().is_ok());
    }

    #[test]
    fn restore_requires_a_target() {
        assert!(parse(&["broza", "restore"]).validate().is_err());
        assert!(parse(&["broza", "restore", "--list"]).validate().is_ok());
        assert!(parse(&["broza", "restore", "--all"]).validate().is_ok());
        assert!(parse(&["broza", "restore", "cln_1"]).validate().is_ok());
        assert!(parse(&["broza", "restore", "--session", "cln_1"]).validate().is_ok());
    }

    #[test]
    fn quarantine_purge_requires_exactly_one_selection() {
        assert!(parse(&["broza", "quarantine", "purge"]).validate().is_err());
        assert!(parse(&["broza", "quarantine", "purge", "cln_1", "--all"]).validate().is_err());
        assert!(parse(&["broza", "quarantine", "purge", "--all"]).validate().is_ok());
        assert!(parse(&["broza", "quarantine", "purge", "cln_1"]).validate().is_ok());
    }

    #[test]
    fn quarantine_purge_rejects_yes_exactly_like_clean() {
        let purge =
            parse(&["broza", "quarantine", "purge", "--all", "--yes"]).validate().expect_err("must fail");
        let clean = parse(&["broza", "clean", "--purge", "--yes"]).validate().expect_err("must fail");
        assert_eq!(purge.to_string(), clean.to_string());
        assert!(purge.to_string().contains("PURGE"), "{purge}");
    }

    #[test]
    fn only_the_writing_config_subcommands_may_name_an_absent_file() {
        for args in [
            vec!["broza", "config", "set", "min-size", "2GB"],
            vec!["broza", "config", "reset"],
            vec!["broza", "config", "path"],
        ] {
            let cli = parse(&args);
            assert!(cli.command.as_ref().is_some_and(Command::creates_config_file), "{args:?}");
        }
        for args in [
            vec!["broza", "config", "list"],
            vec!["broza", "config", "get", "min-size"],
            vec!["broza", "about"],
            vec!["broza", "scan"],
        ] {
            let cli = parse(&args);
            assert!(!cli.command.as_ref().is_some_and(Command::creates_config_file), "{args:?}");
        }
    }

    #[test]
    fn command_names_match_the_json_envelope() {
        assert_eq!(parse(&["broza", "about"]).command.as_ref().map(Command::name), Some("about"));
        assert_eq!(parse(&["broza", "config", "list"]).command.as_ref().map(Command::name), Some("config"));
    }

    #[test]
    fn unknown_flags_fail_to_parse() {
        assert!(Cli::try_parse_from(["broza", "--nonsense"]).is_err());
        assert!(Cli::try_parse_from(["broza", "suggest", "--risk", "chartreuse"]).is_err());
    }
}
