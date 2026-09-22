//! `broza clean` (`docs/cli-spec.md` §3.4 and §4.4).
//!
//! Dry run by default. The order is the specification's: validate the flags,
//! detect, build the plan, run the safety kernel, confirm, expire the sessions
//! whose retention is over, move the items, report. Every write goes through a
//! token the guard issued; this module holds no way around it.

use std::path::{Path, PathBuf};

use broza::clean::execute as execute_plan;
use broza::clean::planner::{Selection, plan_dry_run};
use broza::config::Config;
use broza::model::{Category, CleanPlan, Host, Risk, Warning};
use broza::ports::Ports;
use broza::quarantine::MoveRequest;
use broza::quarantine::layout::generate_session_id;
use broza::safety::Exclusions;
use broza::safety::guard::{PendingApproval, Verdict, WriteRequest, approve};
use broza::units::ByteSize;
use broza::{BrozaError, ExitCode};
use jiff::Timestamp;

use crate::args::{CleanArgs, RiskLevel};
use crate::commands::clean_expiry::{Expiry, due_sessions, expire_due};
use crate::commands::clean_output::CleanOutput;
use crate::commands::detection::{self, DetectionRequest};
use crate::commands::scan::folders::FolderSettings;
use crate::commands::{Outcome, Reclaimed};
use crate::output::{OutputFormat, Renderer};

/// Everything `run` needs beyond the arguments.
pub struct CleanContext<'a> {
    /// The wired adapters.
    pub ports: &'a Ports,
    /// The `clean` flags as parsed and validated by the CLI.
    pub args: &'a CleanArgs,
    /// The effective configuration (`unused-after`, `quarantine-ttl`, `quarantine-path`, `exclude`).
    pub config: &'a Config,
    /// `host` block of the envelope.
    pub host: Host,
    /// `generated_at` of the envelope.
    pub generated_at: Timestamp,
    /// Warnings raised before the command ran.
    pub warnings: Vec<Warning>,
    /// Format the caller asked for.
    pub format: OutputFormat,
    /// Home, cache and progress settings shared with `scan`.
    pub folders: FolderSettings,
    /// An interactive terminal is available for prompts.
    pub tty: bool,
    /// `CI` is set.
    pub ci: bool,
    /// Per-uid temporary directories the guard may allow.
    pub uid_temp_dirs: Vec<PathBuf>,
}

/// Plan, approve, confirm, execute and render.
///
/// # Errors
///
/// Usage errors (exit `2`) for a selection without `--category` or `--risk`, a
/// flag that does not parse, an exclusion that is not a glob, or `HOME` unset;
/// the guard's rejections with their own exit codes; and whatever the
/// filesystem reports while moving.
pub fn run(context: &CleanContext<'_>) -> Result<Outcome, BrozaError> {
    let home = detection::home_of(&context.folders)?;
    let selection = selection_of(context)?;
    let max_size = context.args.max_size.as_deref().map(str::parse::<ByteSize>).transpose()?;
    let unused_after =
        detection::duration_or(context.args.unused_after.as_deref(), context.config.unused_after)?;
    let detected = detection::detect(&DetectionRequest {
        ports: context.ports,
        folders: &context.folders,
        home,
        now: context.generated_at,
        unused_after,
        categories: selection.categories.as_deref(),
    })?;
    let root = detection::quarantine_root(context.config, home);
    let session_id = generate_session_id(context.ports.clock.as_ref())?;
    let outcome = plan_dry_run(&detected.findings, &selection, session_id, Some(&root))?;
    let request = WriteRequest {
        apply: context.args.apply,
        purge: context.args.purge,
        yes: context.args.yes,
        tty: context.tty,
        ci: context.ci,
        max_size: max_size.map(ByteSize::bytes),
        exclusions: selection.exclusions.clone(),
        quarantine_root: Some(root.clone()),
        home: home.to_path_buf(),
        uid_temp_dirs: context.uid_temp_dirs.clone(),
    };
    let verdict =
        approve(&outcome, &detected.findings, &request, &detected.mounts, context.ports.fs.as_ref())?;
    let execution = Execution { root: &root, max_size, mounts: &detected.mounts };
    let executed = match verdict {
        Verdict::DryRun(plan) => dry_run(context, plan, &root),
        Verdict::Nothing(plan) if !context.args.apply => dry_run(context, plan, &root),
        Verdict::Nothing(plan) => nothing_to_move(context, plan, &execution)?,
        Verdict::NeedsConfirmation(pending) => execute(context, pending, &execution)?,
    };
    let mut warnings = detected.warnings;
    warnings.extend(outcome.informed_in_passing.iter().map(inform_only_skipped));
    render(context, executed, warnings, home)
}

/// Warning code: an inform-only finding of a selected category was left out.
pub const INFORM_ONLY_SKIPPED_CODE: &str = "inform_only_skipped";

/// The warning for a finding Broza only reports, inside a category it cleans.
fn inform_only_skipped(id: &broza::model::FindingId) -> Warning {
    Warning {
        code: INFORM_ONLY_SKIPPED_CODE.to_owned(),
        message: format!(
            "{id} is inform-only and was left out of the plan; `broza explain {}` says what to do",
            id.category_part()
        ),
        path: None,
    }
}

/// What the run leaves for the report.
pub(crate) struct Executed {
    /// The plan, with every item's outcome.
    pub plan: CleanPlan,
    /// `errors[]` of the envelope: what made the run partial.
    pub errors: Vec<Warning>,
    /// Warnings the execution raised.
    pub warnings: Vec<Warning>,
    /// Dry run only: the sessions `--apply` would expire, and their bytes.
    pub due: Vec<(broza::model::SessionId, u64)>,
}

/// What `execute` needs beyond the pending approval.
struct Execution<'a> {
    root: &'a Path,
    max_size: Option<ByteSize>,
    mounts: &'a broza::scan::MountTable,
}

/// `--category` and `--risk` into a selection; one of them is mandatory.
fn selection_of(context: &CleanContext<'_>) -> Result<Selection, BrozaError> {
    let categories = detection::parse_categories(&context.args.categories)?;
    if categories.is_none() && context.args.risk.is_none() {
        return Err(BrozaError::Usage(
            "clean needs --category <ID> or --risk <LEVEL> to say what to clean; `broza suggest` lists both"
                .to_owned(),
        ));
    }
    let patterns: Vec<String> =
        context.config.exclude.iter().cloned().chain(context.args.exclude.iter().cloned()).collect();
    let exclusions = Exclusions::new(patterns)?;
    Ok(Selection {
        categories,
        risk_ceiling: context.args.risk.map(core_risk),
        purge: context.args.purge,
        exclusions,
    })
}

/// The CLI's `--risk` level as the core's.
const fn core_risk(level: RiskLevel) -> Risk {
    match level {
        RiskLevel::Green => Risk::Green,
        RiskLevel::Amber => Risk::Amber,
        RiskLevel::Red => Risk::Red,
    }
}

/// Nothing is written: the plan as it is, plus what `--apply` would expire.
fn dry_run(context: &CleanContext<'_>, plan: CleanPlan, root: &Path) -> Executed {
    let ttl = context.config.quarantine_ttl.to_duration();
    let (due, warnings) = due_sessions(context.ports, root, ttl);
    Executed { plan, errors: Vec::new(), warnings, due }
}

/// `--apply` with nothing left to move: the expiry step still runs, and the
/// items the guard skipped still reach `errors[]`.
fn nothing_to_move(
    context: &CleanContext<'_>,
    plan: CleanPlan,
    execution: &Execution<'_>,
) -> Result<Executed, BrozaError> {
    let ttl = context.config.quarantine_ttl.to_duration();
    let Expiry { sessions, freed_bytes, mut errors, warnings } =
        expire_due(context.ports, execution.root, ttl, execution.mounts, context.args.yes)?;
    errors.extend(item_errors(&plan));
    let plan = plan.with_expired_sessions(sessions)?.with_bytes(0, freed_bytes)?;
    Ok(Executed { plan, errors, warnings, due: Vec::new() })
}

/// Confirm, expire what is due, move the items, fold the numbers together.
fn execute(
    context: &CleanContext<'_>,
    pending: PendingApproval,
    execution: &Execution<'_>,
) -> Result<Executed, BrozaError> {
    let approved = pending.confirm(context.ports.prompter.as_ref())?;
    let ttl = context.config.quarantine_ttl.to_duration();
    let Expiry { sessions, freed_bytes, mut errors, mut warnings } =
        expire_due(context.ports, execution.root, ttl, execution.mounts, context.args.yes)?;
    let request = MoveRequest { ttl, max_size: execution.max_size.map(ByteSize::bytes) };
    let done = execute_plan(
        &approved,
        &request,
        context.ports.fs.as_ref(),
        context.ports.clock.as_ref(),
        context.ports.snapshots.as_ref(),
    )?;
    warnings.extend(done.warnings);
    errors.extend(item_errors(&done.plan));
    let (quarantined, reclaimed) =
        (done.plan.quarantined_bytes(), done.plan.reclaimed_bytes().saturating_add(freed_bytes));
    let plan = done.plan.with_expired_sessions(sessions)?.with_bytes(quarantined, reclaimed)?;
    Ok(Executed { plan, errors, warnings, due: Vec::new() })
}

/// One `errors[]` entry per item that was not moved (`docs/cli-spec.md` §2).
pub(crate) fn item_errors(plan: &CleanPlan) -> Vec<Warning> {
    plan.items()
        .iter()
        .filter(|item| item.status.is_unsuccessful())
        .map(|item| Warning {
            code: item.error.as_ref().map_or_else(|| "item_not_moved".to_owned(), ToString::to_string),
            message: format!("{} was not moved ({})", item.path.display(), item.status.as_str()),
            path: Some(item.path.clone()),
        })
        .collect()
}

/// The `broza clean …` line that reproduces this run's selection, for the
/// dry-run footer: built from the flags, never guessed from the plan.
fn rerun_command(args: &CleanArgs) -> String {
    let mut parts = vec!["broza clean".to_owned()];
    // Categories are emitted as the validated ids, not the raw arguments.
    parts.extend(
        args.categories
            .iter()
            .filter_map(|raw| Category::all().into_iter().find(|c| c.as_str() == raw.trim()))
            .map(|c| format!("--category {}", c.as_str())),
    );
    if let Some(risk) = args.risk {
        let level = match risk {
            RiskLevel::Green => "green",
            RiskLevel::Amber => "amber",
            RiskLevel::Red => "red",
        };
        parts.push(format!("--risk {level}"));
    }
    parts.extend(args.exclude.iter().map(|glob| format!("--exclude {}", shell_quote(glob))));
    if let Some(size) = &args.max_size {
        parts.push(format!("--max-size {}", shell_quote(size)));
    }
    if let Some(period) = &args.unused_after {
        parts.push(format!("--unused-after {}", shell_quote(period)));
    }
    if args.purge {
        parts.push("--purge".to_owned());
    }
    parts.join(" ")
}

/// `value` as one shell word: single-quoted, with embedded quotes escaped.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The envelope, the exit code, the text.
pub(crate) fn render(
    context: &CleanContext<'_>,
    executed: Executed,
    detection_warnings: Vec<Warning>,
    home: &Path,
) -> Result<Outcome, BrozaError> {
    let warnings = [context.warnings.clone(), detection_warnings, executed.warnings].concat();
    let code = if executed.errors.is_empty() { ExitCode::Ok } else { ExitCode::PartialFailure };
    let reclaimed = (!executed.plan.is_dry_run()).then(|| Reclaimed {
        quarantined_bytes: executed.plan.quarantined_bytes(),
        freed_bytes: executed.plan.reclaimed_bytes(),
    });
    let output = CleanOutput {
        plan: executed.plan,
        due: executed.due,
        rerun: rerun_command(context.args),
        errors: executed.errors,
        warnings: warnings.clone(),
        host: context.host.clone(),
        generated_at: context.generated_at,
        home: home.to_path_buf(),
    };
    let outcome = Outcome::ok(output.render(context.format)?).with_warnings(warnings).with_code(code);
    Ok(match reclaimed {
        Some(figures) => outcome.with_reclaimed(figures),
        None => outcome,
    })
}
