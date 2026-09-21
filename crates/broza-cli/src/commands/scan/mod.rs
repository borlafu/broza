//! `broza scan` (`docs/cli-spec.md` §3.1 and §4.2).
//!
//! Enumerate the machine, narrow it to what the flags asked for (the private
//! `select` module), and render it. Nothing here walks a filesystem:
//! `largest_items` stays empty until the folder walker lands, and the flags
//! that would drive it raise the `folder_scan_pending` warning rather than an
//! error, so a script written against the final flag set keeps working today
//! and can see, in the envelope, why its table is empty.
//!
//! Failures are graded. An enumeration that cannot run at all is an error with
//! its own exit code; anything the enumerator or the mount table had to skip
//! is a warning, because a partial map of the machine is still worth printing
//! (`AGENTS.md` §6, `docs/cli-spec.md` §6).

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
/// Stable code of the warning the folder-walking flags raise.
pub const WARNING_FOLDER_SCAN_PENDING: &str = "folder_scan_pending";
/// What that warning says, with the flags that earned it substituted in.
const FOLDER_SCAN_MESSAGE: &str = "{flags} accepted but not used yet: largest_items is empty \
     until folder scan lands in the next release";
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
}

/// Enumerate, narrow and render.
///
/// # Errors
///
/// [`BrozaError::Usage`] (exit `2`) for a blank `--volume`,
/// [`BrozaError::TargetNotFound`] (exit `4`) when it names nothing, and
/// whatever the enumerator returns when it cannot run at all.
pub fn run(context: &ScanContext<'_>) -> Result<Outcome, BrozaError> {
    let enumeration = context.ports.disks.enumerate()?;
    let mount = mount_table(context.ports, &enumeration.disks)?;
    let warnings = [
        context.warnings.clone(),
        enumeration.warnings,
        mount.warnings,
        folder_scan_warning(context.args).into_iter().collect(),
    ]
    .concat();

    let disks = select::select(enumeration.disks, context.args)?;
    let output = ScanOutput {
        data: ScanReport { disks, largest_items: Vec::new() },
        host: context.host.clone(),
        generated_at: context.generated_at,
        warnings: warnings.clone(),
        errors: Vec::new(),
        policy: context.policy,
    };
    let code = output.exit_code();
    Ok(Outcome::ok(output.render(context.format)?).with_warnings(warnings).with_code(code))
}

/// The warning the folder-walking flags raise, if any were supplied.
///
/// It is a warning and not a note on stderr because a `--json` consumer needs
/// the same answer a human gets: `largest_items` is empty by design today, not
/// because this machine has no large files. It disappears from the contract
/// the day the walker is wired.
fn folder_scan_warning(args: &ScanArgs) -> Option<Warning> {
    let requested = requested_folder_flags(args);
    if requested.is_empty() {
        return None;
    }
    Some(Warning {
        code: WARNING_FOLDER_SCAN_PENDING.to_owned(),
        message: FOLDER_SCAN_MESSAGE.replace("{flags}", &requested.join(", ")),
        path: None,
    })
}

/// Which of the folder-walking inputs the user actually supplied.
fn requested_folder_flags(args: &ScanArgs) -> Vec<&'static str> {
    let given = [
        (!args.paths.is_empty(), "PATH"),
        (args.depth != crate::args::scan::DEFAULT_DEPTH, "--depth"),
        (args.top != crate::args::scan::DEFAULT_TOP, "--top"),
        (args.min_size.is_some(), "--min-size"),
        (args.tree, "--tree"),
    ];
    given.into_iter().filter(|(supplied, _)| *supplied).map(|(_, name)| name).collect()
}

/// A rendered `scan`, in any of the three formats.
#[derive(Debug, Clone)]
pub struct ScanOutput {
    /// The payload of `docs/cli-spec.md` §4.2.
    pub data: ScanReport,
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
        human_scan::render(&self.data, self.policy)
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
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;

    use broza::model::{Container, Disk, FsKind, Volume, VolumeId};

    use super::*;

    fn id(raw: &str) -> VolumeId {
        raw.parse().unwrap_or_else(|e| panic!("{raw}: {e}"))
    }

    fn machine() -> Vec<Disk> {
        let volume = |raw: &str, name: &str, role: VolumeRole, mount: Option<&str>| Volume {
            id: id(raw),
            name: name.to_owned(),
            uuid: None,
            role,
            mount_point: mount.map(PathBuf::from),
            used_bytes: 1_000_000_000,
            writable_by_broza: role.writable_by_broza(),
            purpose: String::new(),
        };
        vec![Disk {
            id: id("disk0"),
            model: "APPLE SSD".to_owned(),
            size_bytes: 1_000_000_000_000,
            internal: true,
            containers: vec![Container {
                id: id("disk3"),
                kind: FsKind::Apfs,
                size_bytes: 10_000_000_000,
                used_bytes: 4_000_000_000,
                free_bytes: 6_000_000_000,
                purgeable_bytes: 1_000_000_000,
                volumes: vec![
                    volume("disk3s1", "Macintosh HD", VolumeRole::System, Some("/")),
                    volume("disk3s5", "Data", VolumeRole::Data, Some("/System/Volumes/Data")),
                    volume("disk3s2", "Preboot", VolumeRole::Preboot, None),
                ],
            }],
        }]
    }

    fn args() -> ScanArgs {
        ScanArgs {
            paths: Vec::new(),
            depth: crate::args::scan::DEFAULT_DEPTH,
            top: crate::args::scan::DEFAULT_TOP,
            min_size: None,
            volume: None,
            no_external: false,
            tree: false,
        }
    }

    fn output(disks: Vec<Disk>) -> ScanOutput {
        ScanOutput {
            data: ScanReport { disks, largest_items: Vec::new() },
            host: Host { macos_version: "26.1".into(), arch: "arm64".into() },
            generated_at: "2026-09-21T10:36:08Z".parse().unwrap_or_else(|e| panic!("{e}")),
            warnings: Vec::new(),
            errors: Vec::new(),
            policy: ColorPolicy::Never,
        }
    }

    /// `run` against fakes: no process, no disk, no real filesystem.
    fn run_with(disks: Vec<Disk>, args: &ScanArgs, format: OutputFormat) -> Result<Outcome, BrozaError> {
        let (ports, handles) = broza::testing::fake_ports();
        handles.disks.set_disks(disks);
        handles.fs.add_root("/", 1);
        handles.fs.add_root("/System/Volumes/Data", 2);
        run(&ScanContext {
            ports: &ports,
            args,
            host: Host { macos_version: "26.1".into(), arch: "arm64".into() },
            generated_at: "2026-09-21T10:36:08Z".parse().unwrap_or_else(|e| panic!("{e}")),
            warnings: Vec::new(),
            policy: ColorPolicy::Never,
            format,
        })
    }

    #[test]
    fn a_run_enumerates_renders_and_succeeds() {
        let outcome = run_with(machine(), &args(), OutputFormat::Human).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(outcome.code, ExitCode::Ok);
        assert!(outcome.rendered.contains("Physical disk  disk0"), "{}", outcome.rendered);
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    }

    #[test]
    fn a_run_carries_the_enumerator_s_own_warnings_forward() {
        let (ports, handles) = broza::testing::fake_ports();
        handles.disks.set_disks(machine());
        handles.disks.set_warnings(vec![Warning {
            code: "permission_denied".into(),
            message: "skipped".into(),
            path: None,
        }]);

        let outcome = run(&ScanContext {
            ports: &ports,
            args: &args(),
            host: Host { macos_version: "26.1".into(), arch: "arm64".into() },
            generated_at: "2026-09-21T10:36:08Z".parse().unwrap_or_else(|e| panic!("{e}")),
            warnings: Vec::new(),
            policy: ColorPolicy::Never,
            format: OutputFormat::Json,
        })
        .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(outcome.warnings.len(), 1);
        assert_eq!(outcome.warnings[0].code, "permission_denied");
        assert!(outcome.rendered.contains("permission_denied"), "and it reaches the envelope");
        assert_eq!(outcome.code, ExitCode::Ok, "a warning is never a partial failure");
    }

    #[test]
    fn a_run_that_names_no_volume_fails_without_rendering() {
        let args = ScanArgs { volume: Some("No Such Volume".to_owned()), ..args() };

        let error = run_with(machine(), &args, OutputFormat::Human).expect_err("must fail");

        assert_eq!(ExitCode::from(&error), ExitCode::TargetNotFound);
    }

    #[test]
    fn the_folder_flags_reach_the_outcome_as_a_warning() {
        let args = ScanArgs { tree: true, ..args() };

        let outcome = run_with(machine(), &args, OutputFormat::Json).unwrap_or_else(|e| panic!("{e}"));

        let codes: Vec<&str> = outcome.warnings.iter().map(|w| w.code.as_str()).collect();
        assert_eq!(codes, vec![WARNING_FOLDER_SCAN_PENDING]);
    }

    #[test]
    fn a_csv_run_renders_the_volume_table() {
        let outcome = run_with(machine(), &args(), OutputFormat::Csv).unwrap_or_else(|e| panic!("{e}"));

        assert!(outcome.rendered.starts_with("disk_id,container_id"), "{}", outcome.rendered);
        assert_eq!(outcome.rendered.lines().count(), 4);
    }

    #[test]
    fn the_folder_flags_raise_one_warning_naming_them() {
        let args = ScanArgs { tree: true, min_size: Some("1GB".to_owned()), ..args() };

        let warning = folder_scan_warning(&args).unwrap_or_else(|| panic!("expected a warning"));

        assert_eq!(warning.code, WARNING_FOLDER_SCAN_PENDING);
        assert!(warning.message.contains("--tree") && warning.message.contains("--min-size"));
        assert!(warning.message.contains("largest_items"), "{}", warning.message);
    }

    #[test]
    fn the_default_flags_raise_nothing_at_all() {
        assert!(folder_scan_warning(&args()).is_none());
        assert!(requested_folder_flags(&args()).is_empty());
    }

    #[test]
    fn a_path_argument_also_earns_the_warning() {
        let args = ScanArgs { paths: vec![PathBuf::from("/Users")], ..args() };
        assert_eq!(requested_folder_flags(&args), vec!["PATH"]);
    }

    #[test]
    fn an_explicit_default_counts_as_a_flag_the_user_supplied() {
        let args = ScanArgs { depth: 4, top: 5, ..args() };
        assert_eq!(requested_folder_flags(&args), vec!["--depth", "--top"]);
    }

    #[test]
    fn the_csv_has_one_header_and_one_row_per_volume() {
        let csv = output(machine()).to_csv().unwrap_or_else(|e| panic!("{e}"));

        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(
            lines[0],
            "disk_id,container_id,volume_id,name,role,mount_point,used_bytes,writable_by_broza"
        );
        assert_eq!(lines.len(), 4, "three volumes plus the header: {csv}");
        assert!(lines[1].starts_with("disk0,disk3,disk3s1,Macintosh HD,system,/,"), "{}", lines[1]);
        assert!(lines[2].ends_with(",1000000000,true"), "{}", lines[2]);
    }

    #[test]
    fn a_volume_without_a_mount_point_leaves_the_column_empty() {
        let csv = output(machine()).to_csv().unwrap_or_else(|e| panic!("{e}"));

        assert!(csv.contains("Preboot,preboot,,1000000000,false"), "{csv}");
    }

    #[test]
    fn the_role_column_uses_the_contract_token() {
        assert_eq!(role_token(VolumeRole::System), "system");
        assert_eq!(role_token(VolumeRole::Vm), "vm");
        assert_eq!(role_token(VolumeRole::Unknown), "unknown");
    }

    #[test]
    fn the_json_is_an_envelope_with_an_empty_largest_items() {
        let json = output(machine()).to_json().unwrap_or_else(|e| panic!("{e}"));
        let value: serde_json::Value = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(value["command"], "scan");
        assert_eq!(value["data"]["largest_items"], serde_json::json!([]));
        assert_eq!(value["data"]["disks"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn warnings_travel_inside_the_envelope_and_never_change_the_exit_code() {
        let mut out = output(machine());
        out.warnings = vec![Warning { code: "c".into(), message: "m".into(), path: None }];

        let json = out.to_json().unwrap_or_else(|e| panic!("{e}"));
        let value: serde_json::Value = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(value["warnings"][0]["code"], "c");
        assert_eq!(value["errors"], serde_json::json!([]));
        assert_eq!(out.exit_code(), ExitCode::Ok);
    }

    #[test]
    fn an_error_in_the_envelope_means_a_partial_failure() {
        let mut out = output(machine());
        out.errors = vec![ErrorEntry { code: "io_error".into(), message: "m".into(), path: None }];

        let json = out.to_json().unwrap_or_else(|e| panic!("{e}"));
        let value: serde_json::Value = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(value["errors"][0]["code"], "io_error");
        assert_eq!(out.exit_code(), ExitCode::PartialFailure);
    }
}
