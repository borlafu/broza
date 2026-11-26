//! Tests of `broza explain`: target resolution, findings, rendering.
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
    // `<category> <finding>` in one string; anything else is one target,
    // spaces and all (`No Such Volume`).
    let (target, finding) = match target.split_once(' ') {
        Some((first, finding)) if first.parse::<Category>().is_ok() || first == "disk3s5" => {
            (first, Some(finding.to_owned()))
        }
        _ => (target, None),
    };
    let args = ExplainArgs { target: target.to_owned(), finding, short };
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
fn a_finding_broza_only_reports_is_answered_with_its_steps() {
    let outcome =
        run_with("cloud-synced icloud", false, OutputFormat::Json).unwrap_or_else(|e| panic!("{e}"));
    let json: serde_json::Value = serde_json::from_str(&outcome.rendered).unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(json["data"]["finding"], "cloud-synced.icloud");
    assert_eq!(json["data"]["instructions"]["provider"], "iCloud Drive");
}

#[test]
fn an_unknown_finding_is_not_found_and_names_the_ones_that_exist() {
    let error = run_with("cloud-synced nope", false, OutputFormat::Human).expect_err("no such finding");
    let trash = run_with("trash home", false, OutputFormat::Human).expect_err("trash reports nothing");
    let volume = run_with("disk3s5 icloud", false, OutputFormat::Human).expect_err("not a category");

    assert!(matches!(error, BrozaError::TargetNotFound(ref m) if m.contains("icloud, dropbox")), "{error}");
    assert!(matches!(trash, BrozaError::TargetNotFound(_)), "{trash}");
    assert!(matches!(volume, BrozaError::Usage(_)), "{volume}");
}

#[test]
fn a_category_target_is_answered_without_enumerating_anything() {
    let (ports, handles) = broza::testing::fake_ports();
    let args = ExplainArgs { target: "snapshots".to_owned(), finding: None, short: false };

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

    let value: serde_json::Value = serde_json::from_str(&outcome.rendered).unwrap_or_else(|e| panic!("{e}"));
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
    let args = ExplainArgs { target: "/Users".to_owned(), finding: None, short: false };

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
