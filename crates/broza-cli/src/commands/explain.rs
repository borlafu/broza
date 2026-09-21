//! `broza explain` (`docs/cli-spec.md` §3.2 and §4.7).
//!
//! The target is resolved in the order the specification fixes: a category id
//! first, then a volume, then a filesystem path. The order is what makes the
//! command predictable — `snapshots` is the category even on a machine with a
//! volume of that name — and it is also the cheap-to-expensive order, because
//! only the last two need to enumerate the machine.
//!
//! A path is normalised **lexically**: `.` and `..` are resolved textually and
//! `..` can never climb above the root. Nothing is followed and nothing is
//! opened; asking what a path *would* live on must not require the path to
//! exist, and must not be answerable by planting a symlink.

use std::path::{Component, Path, PathBuf};

use broza::detect::ExplainReport;
use broza::model::{Category, Envelope, Host, Volume, VolumeId, Warning};
use broza::ports::Ports;
use broza::{BrozaError, ExitCode};
use jiff::Timestamp;

use crate::args::ExplainArgs;
use crate::commands::Outcome;
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
        return Ok((ExplainReport::for_category(category), context.warnings.clone()));
    }
    let enumeration = context.ports.disks.enumerate()?;
    let warnings = [context.warnings.clone(), enumeration.warnings].concat();
    if let Some(volume) = volume_named(&enumeration.disks, target) {
        return Ok((ExplainReport::for_volume(volume), warnings));
    }
    let mount = broza::adapters::system_mount_table(&enumeration.disks)?;
    let warnings = [warnings, mount.warnings].concat();
    let path = normalize(Path::new(target), context.cwd);
    let entry =
        mount.table.volume_for(&path).ok_or_else(|| BrozaError::TargetNotFound(unresolved(target)))?;
    Ok((ExplainReport::for_path(&path, entry.volume.clone()), warnings))
}

/// Why nothing matched, in the order the specification resolves targets.
fn unresolved(target: &str) -> String {
    format!(
        "`{target}` is not a category id, a volume, or a path on a volume Broza can see; \
         try `broza scan` to list the volumes"
    )
}

/// The volume `target` names: by BSD id, by Finder name, or by mount point.
fn volume_named(disks: &[broza::model::Disk], target: &str) -> Option<Volume> {
    let wanted: Option<VolumeId> = target.parse().ok();
    disks
        .iter()
        .flat_map(|disk| &disk.containers)
        .flat_map(|container| &container.volumes)
        .find(|volume| {
            wanted.as_ref() == Some(&volume.id)
                || volume.name == target
                || volume.mount_point.as_deref() == Some(Path::new(target))
        })
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
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use broza::detect::ExplainKind;
    use broza::model::{Container, Disk, FsKind, VolumeRole};

    use super::*;

    fn id(raw: &str) -> VolumeId {
        raw.parse().unwrap_or_else(|e| panic!("{raw}: {e}"))
    }

    fn volume(raw: &str, name: &str, role: VolumeRole, mount: Option<&str>) -> Volume {
        Volume {
            id: id(raw),
            name: name.to_owned(),
            role,
            mount_point: mount.map(PathBuf::from),
            used_bytes: 1_000_000_000,
            writable_by_broza: role.writable_by_broza(),
            purpose: "A volume.".to_owned(),
        }
    }

    fn disks() -> Vec<Disk> {
        vec![Disk {
            id: id("disk0"),
            model: "APPLE SSD".to_owned(),
            size_bytes: 1_000_000_000_000,
            internal: true,
            containers: vec![Container {
                id: id("disk3"),
                kind: FsKind::Apfs,
                size_bytes: 1_000_000_000_000,
                used_bytes: 1,
                free_bytes: 1,
                purgeable_bytes: 0,
                volumes: vec![
                    volume("disk3s1", "Macintosh HD", VolumeRole::System, Some("/")),
                    volume("disk3s5", "Macintosh HD - Data", VolumeRole::Data, Some("/System/Volumes/Data")),
                    volume("disk3s2", "Preboot", VolumeRole::Preboot, None),
                ],
            }],
        }]
    }

    fn output(data: ExplainReport, short: bool) -> ExplainOutput {
        ExplainOutput {
            data,
            host: Host { macos_version: "26.1".into(), arch: "arm64".into() },
            generated_at: "2026-09-21T10:36:08Z".parse().unwrap_or_else(|e| panic!("{e}")),
            warnings: Vec::new(),
            policy: ColorPolicy::Never,
            short,
        }
    }

    #[test]
    fn a_volume_is_found_by_id_by_name_or_by_mount_point() {
        for target in ["disk3s5", "Macintosh HD - Data", "/System/Volumes/Data"] {
            let found = volume_named(&disks(), target);
            assert_eq!(found.map(|v| v.id.to_string()), Some("disk3s5".to_owned()), "{target}");
        }
    }

    #[test]
    fn an_unmounted_volume_is_still_found_by_its_id() {
        assert_eq!(volume_named(&disks(), "Preboot").map(|v| v.role), Some(VolumeRole::Preboot));
        assert_eq!(volume_named(&disks(), "disk3s2").map(|v| v.role), Some(VolumeRole::Preboot));
    }

    #[test]
    fn a_target_that_names_nothing_is_not_found() {
        assert!(volume_named(&disks(), "disk9s9").is_none());
        assert!(volume_named(&disks(), "not a device").is_none());
    }

    #[test]
    fn an_absolute_path_is_cleaned_but_not_moved() {
        assert_eq!(normalize(Path::new("/Users/dana"), None), PathBuf::from("/Users/dana"));
        assert_eq!(normalize(Path::new("/Users/./dana/"), None), PathBuf::from("/Users/dana"));
        assert_eq!(normalize(Path::new("/Users/dana/../erin"), None), PathBuf::from("/Users/erin"));
    }

    #[test]
    fn a_relative_path_is_resolved_against_the_working_directory() {
        let cwd = PathBuf::from("/Users/dana");
        assert_eq!(normalize(Path::new("Documents"), Some(&cwd)), PathBuf::from("/Users/dana/Documents"));
        assert_eq!(normalize(Path::new("./x"), Some(&cwd)), PathBuf::from("/Users/dana/x"));
        assert_eq!(normalize(Path::new(".."), Some(&cwd)), PathBuf::from("/Users"));
    }

    #[test]
    fn a_parent_reference_can_never_climb_above_the_root() {
        for raw in ["/../../etc", "../../../../../etc", "/..", ".."] {
            let resolved = normalize(Path::new(raw), Some(Path::new("/")));
            assert!(resolved.starts_with("/"), "{raw} -> {resolved:?}");
            assert!(!resolved.to_string_lossy().contains(".."), "{raw} -> {resolved:?}");
        }
        assert_eq!(normalize(Path::new("/../../etc"), None), PathBuf::from("/etc"));
    }

    #[test]
    fn a_relative_path_without_a_working_directory_falls_back_to_the_root() {
        assert_eq!(normalize(Path::new("Users"), None), PathBuf::from("/Users"));
    }

    #[test]
    fn the_json_is_an_envelope_around_the_explain_payload() {
        let report = ExplainReport::for_category(Category::Snapshots);

        let json = output(report, false).to_json().unwrap_or_else(|e| panic!("{e}"));
        let value: serde_json::Value = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(value["command"], "explain");
        assert_eq!(value["data"]["kind"], "category");
        assert_eq!(value["data"]["risk"], "amber");
        assert_eq!(value["data"]["action"], "tmutil_delete");
        assert!(value["data"]["explanation"]["is_it_safe"].is_string());
    }

    #[test]
    fn a_path_payload_carries_the_path_and_its_volume() {
        let report = ExplainReport::for_path(
            Path::new("/Users"),
            volume("disk3s5", "Data", VolumeRole::Data, Some("/System/Volumes/Data")),
        );

        let json = output(report, false).to_json().unwrap_or_else(|e| panic!("{e}"));
        let value: serde_json::Value = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(value["data"]["kind"], "path");
        assert_eq!(value["data"]["path"], "/Users");
        assert_eq!(value["data"]["volume"]["id"], "disk3s5");
        assert!(value["data"].get("category").is_none(), "{value}");
    }

    #[test]
    fn short_renders_one_line_and_long_renders_sections() {
        let report = ExplainReport::for_category(Category::Trash);

        let short = output(report.clone(), true).to_human();
        let long = output(report, false).to_human();

        assert!(!short.contains('\n'), "{short}");
        assert!(long.contains("What it is:"), "{long}");
        assert!(long.lines().count() > 6, "{long}");
    }

    #[test]
    fn explain_has_no_csv_form() {
        let report = ExplainReport::for_category(Category::Trash);
        let error = output(report, false).to_csv().expect_err("no CSV form");
        assert_eq!(ExitCode::from(&error), ExitCode::UsageError);
    }

    #[test]
    fn an_unresolved_target_explains_the_resolution_order() {
        let message = unresolved("nope");
        assert!(message.contains("category"), "{message}");
        assert!(message.contains("volume"), "{message}");
        assert!(message.contains("broza scan"), "{message}");
    }

    #[test]
    fn a_category_kind_is_what_a_category_target_produces() {
        assert_eq!(ExplainReport::for_category(Category::UserCache).kind, ExplainKind::Category);
    }
}
