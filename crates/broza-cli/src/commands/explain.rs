//! `broza explain` (`docs/cli-spec.md` §3.2 and §4.7).
//!
//! The target is resolved in the order the specification fixes: a category id
//! first, then a volume, then a filesystem path. The order is what makes the
//! command predictable — `snapshots` is the category even on a machine with a
//! volume of that name — and it is also the cheap-to-expensive order, because
//! only the last two need to enumerate the machine.
//!
//! A volume is named the way `scan --volume` names one — by BSD id, Finder
//! name or mount point, through the shared matcher in
//! [`crate::commands::target`] — so a name that works in one command works in
//! the other.
//!
//! A path is normalised **lexically**: `.` and `..` are resolved textually and
//! `..` can never climb above the root, so no target can name anything above
//! `/` and no symlink can redirect the answer. It must then exist, because
//! every string normalises to *some* path and a typo must be an exit `4`
//! rather than a confident description of the working directory's volume. What
//! is explained is the volume the path lives on, and the human output says
//! which volume that is.
//!
//! Output is the three-section human form, `--short`'s single line, or the
//! `--json` envelope of §4.7. There is no `--csv`: an explanation is prose,
//! not a table, and `Cli::validate` rejects the flag before this module runs.
//!
//! Nothing here writes: `explain` reads the disk layout and the filesystem
//! only to say what it found.

use std::path::{Component, Path, PathBuf};

use broza::detect::ExplainReport;
use broza::model::{Category, Disk, Envelope, FsKind, Host, Volume, Warning};
use broza::ports::Ports;
use broza::{BrozaError, ExitCode};
use jiff::Timestamp;

use crate::args::ExplainArgs;
use crate::commands::Outcome;
use crate::commands::mount::mount_table;
use crate::commands::target::matches_volume;
use crate::output::human::explain as human_explain;
use crate::output::{ColorPolicy, OutputFormat, Renderer, envelope_to_json};

/// Command name in the JSON envelope.
const COMMAND: &str = "explain";

/// Everything `run` needs beyond the arguments.
pub struct ExplainContext<'a> {
    /// The wired adapters.
    pub ports: &'a Ports,
    /// The `explain` flags as parsed.
    pub args: &'a ExplainArgs,
    /// Working directory, for a relative path target.
    pub cwd: Option<&'a Path>,
    /// `host` block of the envelope.
    pub host: Host,
    /// `generated_at` of the envelope.
    pub generated_at: Timestamp,
    /// Warnings raised before the command ran.
    pub warnings: Vec<Warning>,
    /// Whether the human rendering may use colour.
    pub policy: ColorPolicy,
    /// Format the caller asked for.
    pub format: OutputFormat,
}

/// Resolve the target and explain it.
///
/// # Errors
///
/// [`BrozaError::TargetNotFound`] (exit `4`) when the target is neither a
/// category, nor a volume, nor a path on a volume Broza can see, and whatever
/// the enumerator returns when it cannot run.
pub fn run(context: &ExplainContext<'_>) -> Result<Outcome, BrozaError> {
    let (report, warnings) = resolve(context)?;
    let output = ExplainOutput {
        data: report,
        host: context.host.clone(),
        generated_at: context.generated_at,
        warnings: warnings.clone(),
        policy: context.policy,
        short: context.args.short,
    };
    Ok(Outcome::ok(output.render(context.format)?).with_warnings(warnings).with_code(ExitCode::Ok))
}

/// The report for the target, plus every warning gathered on the way to it.
fn resolve(context: &ExplainContext<'_>) -> Result<(ExplainReport, Vec<Warning>), BrozaError> {
    let target = context.args.target.as_str();
    if let Ok(category) = target.parse::<Category>() {
        let report = match context.args.finding.as_deref() {
            None => ExplainReport::for_category(category),
            Some(name) => ExplainReport::for_finding(category, name)
                .ok_or_else(|| BrozaError::TargetNotFound(unknown_finding(category, name)))?,
        };
        return Ok((report, context.warnings.clone()));
    }
    if context.args.finding.is_some() {
        return Err(BrozaError::Usage(format!(
            "a finding can only follow a category id, and `{target}` is not one; try `broza explain cloud-synced icloud`"
        )));
    }
    let enumeration = context.ports.disks.enumerate()?;
    let warnings = [context.warnings.clone(), enumeration.warnings].concat();
    if let Some(volume) = volume_named(&enumeration.disks, target) {
        let report = ExplainReport::for_volume(volume.clone());
        return Ok((with_filesystem(report, &enumeration.disks, &volume), warnings));
    }
    let path = normalize(Path::new(target), context.cwd);
    if !context.ports.fs.exists(&path) {
        return Err(BrozaError::TargetNotFound(unresolved(target)));
    }
    let mount = mount_table(context.ports, &enumeration.disks)?;
    let warnings = [warnings, mount.warnings].concat();
    let entry =
        mount.table.volume_for(&path).ok_or_else(|| BrozaError::TargetNotFound(unresolved(target)))?;
    let volume = entry.volume.clone();
    let report = ExplainReport::for_path(&path, volume.clone());
    Ok((with_filesystem(report, &enumeration.disks, &volume), warnings))
}

/// Record which filesystem `volume`'s container uses, when `disks` knows.
///
/// The header says `APFS role:` or `HFS+ role:` from this; without it the
/// command would call an `HFS+` volume on a disk image an APFS one.
fn with_filesystem(report: ExplainReport, disks: &[Disk], volume: &Volume) -> ExplainReport {
    match filesystem_of(disks, volume) {
        Some(kind) => report.on_filesystem(kind),
        None => report,
    }
}

/// The `type` of the container `volume` belongs to.
fn filesystem_of(disks: &[Disk], volume: &Volume) -> Option<FsKind> {
    disks
        .iter()
        .flat_map(|disk| &disk.containers)
        .find(|container| container.volumes.iter().any(|candidate| candidate.id == volume.id))
        .map(|container| container.kind.clone())
}

/// Why nothing matched, in the order the specification resolves targets.
///
/// A path is only explained when it exists: every string normalises to *some*
/// path under `/`, so without that check a typo would be answered with a
/// confident explanation of the volume the working directory happens to be on
/// instead of the exit `4` the specification asks for.
fn unresolved(target: &str) -> String {
    format!(
        "`{target}` is not a category id, a volume, or an existing path on a volume Broza can see; \
         try `broza scan` to list the volumes"
    )
}

/// Why `explain <category> <name>` found nothing, with the names that work.
fn unknown_finding(category: Category, name: &str) -> String {
    let known = broza::detect::reported_findings(category);
    if known.is_empty() {
        return format!(
            "{} has no finding Broza only reports; `broza explain {}` says how to clean it",
            category.as_str(),
            category.as_str()
        );
    }
    format!(
        "{} has no finding `{name}` Broza only reports; try one of: {}",
        category.as_str(),
        known.join(", ")
    )
}

/// The volume `target` names: by BSD id, by Finder name, or by mount point.
fn volume_named(disks: &[Disk], target: &str) -> Option<Volume> {
    disks
        .iter()
        .flat_map(|disk| &disk.containers)
        .flat_map(|container| &container.volumes)
        .find(|volume| matches_volume(volume, target))
        .cloned()
}

/// `path` made absolute against `cwd` and cleaned of `.` and `..`, textually.
///
/// `..` at the root is dropped rather than escaping it, so no target can name
/// anything above `/`. A relative path with no working directory is resolved
/// against the root, which is the only answer that cannot surprise.
pub fn normalize(path: &Path, cwd: Option<&Path>) -> PathBuf {
    let absolute =
        if path.is_absolute() { path.to_path_buf() } else { cwd.unwrap_or(Path::new("/")).join(path) };
    let mut cleaned = PathBuf::from("/");
    for component in absolute.components() {
        match component {
            Component::Normal(part) => cleaned.push(part),
            Component::ParentDir => {
                cleaned.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    cleaned
}

/// A rendered `explain`, in either of its two formats.
#[derive(Debug, Clone)]
pub struct ExplainOutput {
    /// The payload of `docs/cli-spec.md` §4.7.
    pub data: ExplainReport,
    /// `host` block of the envelope.
    pub host: Host,
    /// `generated_at` of the envelope.
    pub generated_at: Timestamp,
    /// Warnings to carry inside the envelope.
    pub warnings: Vec<Warning>,
    /// Whether the human rendering may use colour.
    pub policy: ColorPolicy,
    /// `--short`: one line instead of three sections.
    pub short: bool,
}

impl Renderer for ExplainOutput {
    fn to_human(&self) -> String {
        if self.short {
            human_explain::render_short(&self.data, self.policy)
        } else {
            human_explain::render(&self.data, self.policy)
        }
    }

    fn to_json(&self) -> Result<String, BrozaError> {
        let envelope = Envelope::new(COMMAND, self.host.clone(), self.generated_at, self.data.clone());
        let envelope = self.warnings.iter().cloned().fold(envelope, Envelope::with_warning);
        envelope_to_json(&envelope)
    }
}

#[cfg(test)]
#[path = "explain_tests.rs"]
mod tests;
