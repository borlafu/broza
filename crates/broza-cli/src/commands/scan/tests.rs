//! Tests of the `scan` command against the fakes: no process, no disk, no real filesystem.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use broza::model::{Container, Disk, FsKind, Volume, VolumeId};

use super::*;
use broza::model::VolumeRole;

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

fn no_folders() -> FolderSettings {
    FolderSettings {
        home: None,
        cache_ttl: std::time::Duration::from_secs(60),
        no_cache: true,
        show_progress: false,
        verbose: false,
        own_stores: Vec::new(),
    }
}

fn output(disks: Vec<Disk>) -> ScanOutput {
    ScanOutput {
        data: ScanReport { disks, largest_items: Vec::new() },
        trees: Vec::new(),
        upper_bounds: false,
        tree_view: false,
        home: None,
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
        folders: no_folders(),
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

    handles.fs.add_root("/", 1);
    handles.fs.add_root("/System/Volumes/Data", 2);
    let outcome = run(&ScanContext {
        ports: &ports,
        args: &args(),
        host: Host { macos_version: "26.1".into(), arch: "arm64".into() },
        generated_at: "2026-09-21T10:36:08Z".parse().unwrap_or_else(|e| panic!("{e}")),
        warnings: Vec::new(),
        policy: ColorPolicy::Never,
        format: OutputFormat::Json,
        folders: no_folders(),
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
fn a_run_walks_the_writable_volumes_and_lists_their_largest_items() {
    let (ports, handles) = broza::testing::fake_ports();
    handles.disks.set_disks(machine());
    handles.fs.add_root("/", 1);
    handles.fs.add_root("/System/Volumes/Data", 2);
    handles.fs.add_file("/System/Volumes/Data/Users/dana/big.bin", &[]);
    handles.fs.set_size("/System/Volumes/Data/Users/dana/big.bin", 5_000_000_000);
    let args = ScanArgs { min_size: Some("1GB".to_owned()), ..args() };

    let outcome = run(&ScanContext {
        ports: &ports,
        args: &args,
        host: Host { macos_version: "26.1".into(), arch: "arm64".into() },
        generated_at: "2026-09-21T10:36:08Z".parse().unwrap_or_else(|e| panic!("{e}")),
        warnings: Vec::new(),
        policy: ColorPolicy::Never,
        format: OutputFormat::Human,
        folders: FolderSettings {
            home: Some(PathBuf::from("/Users/dana")),
            cache_ttl: std::time::Duration::from_secs(60),
            no_cache: true,
            show_progress: false,
            verbose: false,
            own_stores: Vec::new(),
        },
    })
    .unwrap_or_else(|e| panic!("{e}"));

    assert!(outcome.rendered.contains("Largest consumers on"), "{}", outcome.rendered);
    assert!(outcome.rendered.contains("5.0 GB  ~/big.bin"), "{}", outcome.rendered);
}

#[test]
fn a_csv_run_renders_the_volume_table() {
    let outcome = run_with(machine(), &args(), OutputFormat::Csv).unwrap_or_else(|e| panic!("{e}"));

    assert!(outcome.rendered.starts_with("disk_id,container_id"), "{}", outcome.rendered);
    assert_eq!(outcome.rendered.lines().count(), 4);
}

#[test]
fn the_csv_has_one_header_and_one_row_per_volume() {
    let csv = output(machine()).to_csv().unwrap_or_else(|e| panic!("{e}"));

    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines[0], "disk_id,container_id,volume_id,name,role,mount_point,used_bytes,writable_by_broza");
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

#[test]
fn the_upper_bounds_note_is_printed_when_the_walk_overcounted() {
    use crate::commands::scan::folders::VolumeTree;
    use broza::scan::{TreeNode, TreeView};
    let mut out = output(machine());
    out.upper_bounds = true;
    out.trees = vec![VolumeTree {
        volume_id: id("disk3s5"),
        name: "Data".to_owned(),
        tree: TreeView {
            root: TreeNode {
                name: "/".into(),
                path: PathBuf::from("/"),
                size_bytes: 0,
                percent_of_parent: 100.0,
                other_bytes: 0,
                children: Vec::new(),
            },
        },
    }];

    let text = out.to_human();

    assert!(text.contains("Sizes below are upper bounds"), "{text}");
}
