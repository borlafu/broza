//! `broza scan` (`docs/cli-spec.md` §3.1 and §4.2).
//!
//! Enumerate the machine, narrow it to what the flags asked for (the private
//! `select` module), walk the volumes that survived (the [`folders`] module),
//! and render it all.
//!
//! Failures are graded. An enumeration that cannot run at all is an error with
//! its own exit code; anything the enumerator or the mount table had to skip
//! is a warning, because a partial map of the machine is still worth printing
//! (`AGENTS.md` §6, `docs/cli-spec.md` §6).

pub mod folders;
mod select;

use broza::model::{Envelope, ErrorEntry, Host, ScanReport, VolumeRole, Warning};
use broza::ports::Ports;
use broza::{BrozaError, ExitCode};
use jiff::Timestamp;

use crate::args::ScanArgs;
use crate::commands::Outcome;
use crate::commands::mount::mount_table;
use crate::output::human::scan as human_scan;
use crate::output::{ColorPolicy, OutputFormat, Renderer, csv, envelope_to_json};
use folders::{FolderSettings, VolumeTree};

/// Command name in the JSON envelope.
const COMMAND: &str = "scan";
/// Header of the `--csv` table (`docs/cli-spec.md` §3.1).
const CSV_HEADER: [&str; 8] = [
    "disk_id",
    "container_id",
    "volume_id",
    "name",
    "role",
    "mount_point",
    "used_bytes",
    "writable_by_broza",
];
/// Role token used when a role somehow fails to serialise.
const UNKNOWN_ROLE_TOKEN: &str = "unknown";

/// Everything `run` needs beyond the arguments.
pub struct ScanContext<'a> {
    /// The wired adapters.
    pub ports: &'a Ports,
    /// The `scan` flags as parsed.
    pub args: &'a ScanArgs,
    /// `host` block of the envelope.
    pub host: Host,
    /// `generated_at` of the envelope.
    pub generated_at: Timestamp,
    /// Warnings raised before the command ran, such as an unknown host version.
    pub warnings: Vec<Warning>,
    /// Whether the human rendering may use colour.
    pub policy: ColorPolicy,
    /// Format the caller asked for.
    pub format: OutputFormat,
    /// Home, cache and progress settings of the folder walk.
    pub folders: FolderSettings,
}

/// Enumerate, narrow, walk and render.
///
/// # Errors
///
/// [`BrozaError::Usage`] (exit `2`) for a blank `--volume`,
/// [`BrozaError::TargetNotFound`] (exit `4`) when it names nothing, and
/// whatever the enumerator returns when it cannot run at all.
pub fn run(context: &ScanContext<'_>) -> Result<Outcome, BrozaError> {
    let enumeration = context.ports.disks.enumerate()?;
    let mount = mount_table(context.ports, &enumeration.disks)?;
    let disks = select::select(enumeration.disks, context.args)?;
    let walked = folders::walk(&disks, &mount.table, context.args, &context.folders, context.ports)?;
    let warnings = [context.warnings.clone(), enumeration.warnings, mount.warnings, walked.warnings].concat();
    let output = ScanOutput {
        data: ScanReport { disks, largest_items: walked.largest },
        trees: walked.trees,
        tree_view: context.args.tree,
        home: context.folders.home.clone(),
        host: context.host.clone(),
        generated_at: context.generated_at,
        warnings: warnings.clone(),
        errors: Vec::new(),
        policy: context.policy,
    };
    let code = output.exit_code();
    Ok(Outcome::ok(output.render(context.format)?).with_warnings(warnings).with_code(code))
}

/// A rendered `scan`, in any of the three formats.
#[derive(Debug, Clone)]
pub struct ScanOutput {
    /// The payload of `docs/cli-spec.md` §4.2.
    pub data: ScanReport,
    /// One tree per walked volume, for the human rendering.
    pub trees: Vec<VolumeTree>,
    /// `--tree`: draw the trees instead of the consumers list.
    pub tree_view: bool,
    /// Home directory, so paths under it print as `~/…`.
    pub home: Option<std::path::PathBuf>,
    /// `host` block of the envelope.
    pub host: Host,
    /// `generated_at` of the envelope.
    pub generated_at: Timestamp,
    /// Warnings to carry inside the envelope.
    pub warnings: Vec<Warning>,
    /// Errors to carry inside the envelope; a non-empty list means exit `5`.
    pub errors: Vec<ErrorEntry>,
    /// Whether the human rendering may use colour.
    pub policy: ColorPolicy,
}

impl ScanOutput {
    /// Exit `5` as soon as the envelope carries an error (`docs/cli-spec.md` §4.1).
    ///
    /// A warning never changes the exit code; only `errors[]` does. `scan`
    /// raises none of its own yet, and the rule is wired here so that the day
    /// a walker does, the contract is already honoured.
    pub fn exit_code(&self) -> ExitCode {
        if self.errors.is_empty() { ExitCode::Ok } else { ExitCode::PartialFailure }
    }

    /// The envelope this output serialises to.
    fn envelope(&self) -> Envelope<ScanReport> {
        let envelope = Envelope::new(COMMAND, self.host.clone(), self.generated_at, self.data.clone());
        let envelope = self.warnings.iter().cloned().fold(envelope, Envelope::with_warning);
        self.errors.iter().cloned().fold(envelope, Envelope::with_error)
    }
}

impl Renderer for ScanOutput {
    fn to_human(&self) -> String {
        human_scan::render_with_folders(
            &self.data,
            &self.trees,
            self.tree_view,
            self.home.as_deref(),
            self.policy,
        )
    }

    fn to_json(&self) -> Result<String, BrozaError> {
        envelope_to_json(&self.envelope())
    }

    /// One row per volume (`docs/cli-spec.md` §3.1).
    fn to_csv(&self) -> Result<String, BrozaError> {
        let header = csv::row(&CSV_HEADER);
        let rows = self.data.disks.iter().flat_map(|disk| {
            disk.containers.iter().flat_map(move |container| {
                container.volumes.iter().map(move |volume| {
                    csv::row(&[
                        disk.id.to_string(),
                        container.id.to_string(),
                        volume.id.to_string(),
                        volume.name.clone(),
                        role_token(volume.role),
                        volume.mount_point.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
                        volume.used_bytes.to_string(),
                        volume.writable_by_broza.to_string(),
                    ])
                })
            })
        });
        Ok(std::iter::once(header).chain(rows).collect::<Vec<_>>().join("\n"))
    }
}

/// The contract token of a role, taken from the contract itself.
fn role_token(role: VolumeRole) -> String {
    serde_json::to_value(role)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| UNKNOWN_ROLE_TOKEN.to_owned())
}

#[cfg(test)]
mod tests;
