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
        return Ok((ExplainReport::for_category(category), context.warnings.clone()));
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
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use broza::detect::ExplainKind;
    use broza::model::{Container, Disk, FsKind, VolumeId, VolumeRole};

    use super::*;

    fn id(raw: &str) -> VolumeId {
        raw.parse().unwrap_or_else(|e| panic!("{raw}: {e}"))
    }

    fn volume(raw: &str, name: &str, role: VolumeRole, mount: Option<&str>) -> Volume {
        Volume {
            id: id(raw),
            name: name.to_owned(),
            uuid: None,
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

    /// `run` against fakes: no process, no disk, no real filesystem.
    fn run_with(target: &str, short: bool, format: OutputFormat) -> Result<Outcome, BrozaError> {
        let (ports, handles) = broza::testing::fake_ports();
        handles.disks.set_disks(disks());
        handles.fs.add_root("/", 1);
        handles.fs.add_root("/System/Volumes/Data", 2);
        handles.fs.add_dir("/Users");
        handles.fs.add_file("/usr/share/firmlinks", b"/Users\tUsers\n");
        let args = ExplainArgs { target: target.to_owned(), short };
        run(&ExplainContext {
            ports: &ports,
            args: &args,
            cwd: Some(Path::new("/")),
            host: Host { macos_version: "26.1".into(), arch: "arm64".into() },
            generated_at: "2026-09-21T10:36:08Z".parse().unwrap_or_else(|e| panic!("{e}")),
            warnings: Vec::new(),
            policy: ColorPolicy::Never,
            format,
        })
    }

    #[test]
    fn a_category_target_is_answered_without_enumerating_anything() {
        let (ports, handles) = broza::testing::fake_ports();
        let args = ExplainArgs { target: "snapshots".to_owned(), short: false };

        let outcome = run(&ExplainContext {
            ports: &ports,
            args: &args,
            cwd: None,
            host: Host { macos_version: "26.1".into(), arch: "arm64".into() },
            generated_at: "2026-09-21T10:36:08Z".parse().unwrap_or_else(|e| panic!("{e}")),
            warnings: Vec::new(),
            policy: ColorPolicy::Never,
            format: OutputFormat::Human,
        })
        .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(outcome.code, ExitCode::Ok);
        assert!(outcome.rendered.starts_with("snapshots"), "{}", outcome.rendered);
        assert!(handles.process.calls().is_empty(), "a category needs no command at all");
    }

    #[test]
    fn a_volume_target_is_answered_with_the_filesystem_of_its_container() {
        let outcome = run_with("disk3s5", false, OutputFormat::Human).unwrap_or_else(|e| panic!("{e}"));

        assert!(outcome.rendered.contains("APFS role: Data"), "{}", outcome.rendered);
    }

    #[test]
    fn a_path_target_is_answered_through_the_mount_table() {
        let outcome = run_with("/Users", false, OutputFormat::Json).unwrap_or_else(|e| panic!("{e}"));

        let value: serde_json::Value =
            serde_json::from_str(&outcome.rendered).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(value["data"]["kind"], "path");
        assert_eq!(value["data"]["path"], "/Users");
        assert_eq!(value["data"]["volume"]["id"], "disk3s5", "the firmlink sent it to the data volume");
        assert_eq!(value["data"]["filesystem"], "apfs");
    }

    #[test]
    fn a_short_run_is_one_line() {
        let outcome = run_with("disk3s5", true, OutputFormat::Human).unwrap_or_else(|e| panic!("{e}"));

        assert!(!outcome.rendered.contains('\n'), "{}", outcome.rendered);
    }

    #[test]
    fn a_target_that_exists_nowhere_is_not_found() {
        for target in ["disk9s9", "No Such Volume", "/nowhere/at/all"] {
            let error = run_with(target, false, OutputFormat::Human).expect_err("must fail");
            assert_eq!(ExitCode::from(&error), ExitCode::TargetNotFound, "{target}");
        }
    }

    #[test]
    fn a_path_on_a_volume_the_mount_table_does_not_know_is_not_found() {
        let (ports, handles) = broza::testing::fake_ports();
        // A machine whose only volume is unmounted: the path exists, and no
        // volume can claim it.
        let mut unmounted = disks();
        unmounted[0].containers[0].volumes = vec![volume("disk3s2", "Preboot", VolumeRole::Preboot, None)];
        handles.disks.set_disks(unmounted);
        handles.fs.add_dir("/Users");
        let args = ExplainArgs { target: "/Users".to_owned(), short: false };

        let error = run(&ExplainContext {
            ports: &ports,
            args: &args,
            cwd: None,
            host: Host { macos_version: "26.1".into(), arch: "arm64".into() },
            generated_at: "2026-09-21T10:36:08Z".parse().unwrap_or_else(|e| panic!("{e}")),
            warnings: Vec::new(),
            policy: ColorPolicy::Never,
            format: OutputFormat::Human,
        })
        .expect_err("must fail");

        assert_eq!(ExitCode::from(&error), ExitCode::TargetNotFound);
    }

    #[test]
    fn the_filesystem_of_a_volume_no_container_claims_is_unknown() {
        let orphan = volume("disk9s9", "Ghost", VolumeRole::User, None);
        assert_eq!(filesystem_of(&disks(), &orphan), None);
        assert_eq!(with_filesystem(ExplainReport::for_volume(orphan.clone()), &[], &orphan).filesystem, None);
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
        assert!(message.contains("existing path"), "{message}");
        assert!(message.contains("broza scan"), "{message}");
    }

    #[test]
    fn a_category_kind_is_what_a_category_target_produces() {
        assert_eq!(ExplainReport::for_category(Category::UserCache).kind, ExplainKind::Category);
    }
}
