//! `broza scan` (`docs/cli-spec.md` §3.1 and §4.2).
//!
//! Enumerate the machine, filter it to what the flags asked for, and render it.
//! Nothing here walks a filesystem: `largest_items` stays empty until the
//! folder walker lands, and the flags that would drive it produce a note on
//! stderr rather than an error, so a script written against the final flag set
//! keeps working today.
//!
//! Failures are graded. An enumeration that cannot run at all is an error with
//! its own exit code; anything the enumerator or the mount table had to skip is
//! a warning, because a partial map of the machine is still worth printing
//! (`AGENTS.md` §6, `docs/cli-spec.md` §6).

use broza::model::{Container, Disk, Envelope, Host, ScanReport, VolumeId, VolumeRole, Warning};
use broza::ports::Ports;
use broza::{BrozaError, ExitCode};
use jiff::Timestamp;

use crate::args::ScanArgs;
use crate::commands::Outcome;
use crate::output::human::scan as human_scan;
use crate::output::{ColorPolicy, OutputFormat, Renderer, csv, envelope_to_json};

/// Command name in the JSON envelope.
const COMMAND: &str = "scan";
/// Header of the `--csv` table.
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
/// What the flags that need the folder walker are told.
const FOLDER_SCAN_NOTE: &str = "accepted but not used yet: folder scan lands in the next release ({flags})";
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

/// Enumerate, filter and render.
///
/// # Errors
///
/// [`BrozaError::Usage`] (exit `2`) for a `--volume` value that is not a BSD
/// device name, [`BrozaError::TargetNotFound`] (exit `4`) when no volume
/// matches it, and whatever the enumerator returns when it cannot run.
pub fn run(context: &ScanContext<'_>) -> Result<Outcome, BrozaError> {
    let enumeration = context.ports.disks.enumerate()?;
    let mount = broza::adapters::system_mount_table(&enumeration.disks)?;
    let warnings = [context.warnings.clone(), enumeration.warnings, mount.warnings].concat();

    let disks = select(enumeration.disks, context.args)?;
    let report = ScanReport { disks, largest_items: Vec::new() };
    let output = ScanOutput {
        data: report,
        host: context.host.clone(),
        generated_at: context.generated_at,
        warnings: warnings.clone(),
        policy: context.policy,
    };
    let rendered = output.render(context.format)?;
    Ok(Outcome::ok(rendered)
        .with_warnings(warnings)
        .with_notes(notes(context.args))
        .with_code(exit_code(&output)))
}

/// Exit `5` as soon as the envelope carries an error (`docs/cli-spec.md` §4.1).
///
/// A warning never changes the exit code; only `errors[]` does. `scan` raises
/// none of its own yet, and the rule is wired here so that the day a detector
/// does, the contract is already honoured.
fn exit_code(output: &ScanOutput) -> ExitCode {
    if output.errors().is_empty() { ExitCode::Ok } else { ExitCode::PartialFailure }
}

/// Apply `--no-external` and `--volume`, in that order.
///
/// The order matters and is the honest one: `--no-external --volume disk4s1`
/// asks for a volume that was just excluded, and answering "not found" is
/// truer than quietly bringing the external disk back.
fn select(disks: Vec<Disk>, args: &ScanArgs) -> Result<Vec<Disk>, BrozaError> {
    let disks = if args.no_external { internal_only(disks) } else { disks };
    let Some(raw) = args.volume.as_deref() else { return Ok(disks) };
    let wanted: VolumeId = raw.parse()?;
    let selected = only(disks, &wanted);
    if selected.is_empty() {
        return Err(BrozaError::TargetNotFound(format!("no disk, container or volume named `{wanted}`")));
    }
    Ok(selected)
}

/// Drop every disk macOS does not report as internal.
fn internal_only(disks: Vec<Disk>) -> Vec<Disk> {
    disks.into_iter().filter(|disk| disk.internal).collect()
}

/// Keep only what `wanted` names, at whatever level it names it.
///
/// macOS puts disks, containers and volumes in one namespace, so `disk0`,
/// `disk3` and `disk3s5` are all legitimate answers to `--volume`; each keeps
/// the level it names and everything below it.
fn only(disks: Vec<Disk>, wanted: &VolumeId) -> Vec<Disk> {
    disks
        .into_iter()
        .filter_map(|disk| {
            if &disk.id == wanted {
                return Some(disk);
            }
            let containers = matching_containers(&disk, wanted);
            (!containers.is_empty()).then_some(Disk { containers, ..disk })
        })
        .collect()
}

/// The containers of `disk` that `wanted` names, directly or through a volume.
fn matching_containers(disk: &Disk, wanted: &VolumeId) -> Vec<Container> {
    disk.containers
        .iter()
        .filter_map(|container| {
            if &container.id == wanted {
                return Some(container.clone());
            }
            let volumes: Vec<_> =
                container.volumes.iter().filter(|volume| &volume.id == wanted).cloned().collect();
            (!volumes.is_empty()).then(|| Container { volumes, ..container.clone() })
        })
        .collect()
}

/// The stderr notes this invocation earned.
fn notes(args: &ScanArgs) -> Vec<String> {
    let requested = requested_folder_flags(args);
    if requested.is_empty() {
        return Vec::new();
    }
    vec![FOLDER_SCAN_NOTE.replace("{flags}", &requested.join(", "))]
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
    /// Whether the human rendering may use colour.
    pub policy: ColorPolicy,
}

impl ScanOutput {
    /// The envelope this output serialises to.
    fn envelope(&self) -> Envelope<ScanReport> {
        let envelope = Envelope::new(COMMAND, self.host.clone(), self.generated_at, self.data.clone());
        self.warnings.iter().cloned().fold(envelope, Envelope::with_warning)
    }

    /// Errors of the envelope; `scan` produces none of its own yet.
    fn errors(&self) -> Vec<broza::model::ErrorEntry> {
        self.envelope().errors
    }
}

impl Renderer for ScanOutput {
    fn to_human(&self) -> String {
        human_scan::render(&self.data, self.policy)
    }

    fn to_json(&self) -> Result<String, BrozaError> {
        envelope_to_json(&self.envelope())
    }

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

    use broza::model::{FsKind, Volume};

    use super::*;

    fn id(raw: &str) -> VolumeId {
        raw.parse().unwrap_or_else(|e| panic!("{raw}: {e}"))
    }

    fn volume(raw: &str, name: &str, role: VolumeRole) -> Volume {
        Volume {
            id: id(raw),
            name: name.to_owned(),
            role,
            mount_point: Some(PathBuf::from("/System/Volumes/Data")),
            used_bytes: 1_000_000_000,
            writable_by_broza: role.writable_by_broza(),
            purpose: String::new(),
        }
    }

    fn container(raw: &str, volumes: Vec<Volume>) -> Container {
        Container {
            id: id(raw),
            kind: FsKind::Apfs,
            size_bytes: 10_000_000_000,
            used_bytes: 4_000_000_000,
            free_bytes: 6_000_000_000,
            purgeable_bytes: 1_000_000_000,
            volumes,
        }
    }

    fn machine() -> Vec<Disk> {
        vec![
            Disk {
                id: id("disk0"),
                model: "APPLE SSD".to_owned(),
                size_bytes: 1_000_000_000_000,
                internal: true,
                containers: vec![container(
                    "disk3",
                    vec![
                        volume("disk3s1", "Macintosh HD", VolumeRole::System),
                        volume("disk3s5", "Macintosh HD - Data", VolumeRole::Data),
                    ],
                )],
            },
            Disk {
                id: id("disk4"),
                model: "Disk Image".to_owned(),
                size_bytes: 2_000_000_000,
                internal: false,
                containers: vec![container("disk4s1", vec![volume("disk4s1", "Ext", VolumeRole::User)])],
            },
        ]
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
            policy: ColorPolicy::Never,
        }
    }

    #[test]
    fn without_flags_every_disk_survives() {
        let selected = select(machine(), &args()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(selected.len(), 2);
    }

    #[test]
    fn no_external_keeps_only_internal_disks() {
        let args = ScanArgs { no_external: true, ..args() };

        let selected = select(machine(), &args).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id.as_str(), "disk0");
    }

    #[test]
    fn a_volume_filter_keeps_that_volume_and_nothing_else() {
        let args = ScanArgs { volume: Some("disk3s5".to_owned()), ..args() };

        let selected = select(machine(), &args).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].containers.len(), 1);
        let names: Vec<&str> = selected[0].containers[0].volumes.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["Macintosh HD - Data"]);
    }

    #[test]
    fn a_container_or_a_disk_may_be_named_too() {
        for (wanted, volumes) in [("disk3", 2), ("disk0", 2)] {
            let args = ScanArgs { volume: Some(wanted.to_owned()), ..args() };
            let selected = select(machine(), &args).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(selected.len(), 1, "{wanted}");
            assert_eq!(selected[0].containers[0].volumes.len(), volumes, "{wanted}");
        }
    }

    #[test]
    fn a_malformed_volume_id_is_a_usage_error_and_a_missing_one_is_not_found() {
        let malformed = ScanArgs { volume: Some("sda1".to_owned()), ..args() };
        let missing = ScanArgs { volume: Some("disk9s9".to_owned()), ..args() };

        assert_eq!(
            ExitCode::from(&select(machine(), &malformed).expect_err("must fail")),
            ExitCode::UsageError
        );
        assert_eq!(
            ExitCode::from(&select(machine(), &missing).expect_err("must fail")),
            ExitCode::TargetNotFound
        );
    }

    #[test]
    fn excluding_externals_makes_an_external_volume_not_found() {
        let args = ScanArgs { no_external: true, volume: Some("disk4s1".to_owned()), ..args() };

        let error = select(machine(), &args).expect_err("must fail");

        assert_eq!(ExitCode::from(&error), ExitCode::TargetNotFound);
    }

    #[test]
    fn the_folder_flags_produce_a_note_naming_them_and_no_error() {
        let args = ScanArgs { tree: true, min_size: Some("1GB".to_owned()), ..args() };

        let notes = notes(&args);

        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("--tree") && notes[0].contains("--min-size"), "{}", notes[0]);
        assert!(notes[0].contains("next release"), "{}", notes[0]);
    }

    #[test]
    fn the_default_flags_produce_no_note_at_all() {
        assert!(notes(&args()).is_empty());
        assert!(requested_folder_flags(&args()).is_empty());
    }

    #[test]
    fn a_path_argument_also_earns_the_note() {
        let args = ScanArgs { paths: vec![PathBuf::from("/Users")], ..args() };
        assert_eq!(requested_folder_flags(&args), vec!["PATH"]);
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
        assert!(lines[1].starts_with("disk0,disk3,disk3s1,Macintosh HD,system,"), "{}", lines[1]);
        assert!(lines[2].ends_with(",1000000000,true"), "{}", lines[2]);
    }

    #[test]
    fn a_volume_without_a_mount_point_leaves_the_column_empty() {
        let mut disks = machine();
        disks[0].containers[0].volumes[0].mount_point = None;

        let csv = output(disks).to_csv().unwrap_or_else(|e| panic!("{e}"));

        assert!(csv.contains("Macintosh HD,system,,1000000000,false"), "{csv}");
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
        assert_eq!(value["data"]["disks"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn warnings_travel_inside_the_envelope() {
        let mut out = output(machine());
        out.warnings = vec![Warning { code: "c".into(), message: "m".into(), path: None }];

        let json = out.to_json().unwrap_or_else(|e| panic!("{e}"));
        let value: serde_json::Value = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(value["warnings"][0]["code"], "c");
        assert_eq!(value["errors"], serde_json::json!([]));
    }

    #[test]
    fn an_empty_envelope_of_errors_means_exit_zero() {
        let mut out = output(machine());
        out.warnings = vec![Warning { code: "c".into(), message: "m".into(), path: None }];

        assert_eq!(exit_code(&out), ExitCode::Ok, "a warning is not a partial failure");
    }
}
