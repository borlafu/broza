//! Tests of `broza clean` against the fake ports: dry run, the confirmation
//! matrix, an applied run, and the pre-execution expiry.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::Duration;

use broza::model::{Container, Disk, FsKind, Volume, VolumeId, VolumeRole};
use broza::ports::{Answer, FileOps};
use broza::testing::Handles;

use crate::args::CleanArgs;
use crate::commands::Outcome;
use crate::commands::clean::{CleanContext, INFORM_ONLY_SKIPPED_CODE, run};
use crate::commands::scan::folders::FolderSettings;
use crate::output::{ColorPolicy, OutputFormat};
use broza::config::Config;
use broza::model::Host;
use broza::ports::Ports;
use broza::{BrozaError, ExitCode};
use std::path::Path;

const HOME: &str = "/System/Volumes/Data/Users/dana";
const CACHE_FILE: &str = "/System/Volumes/Data/Users/dana/Library/Caches/App/c.db";
const STORE: &str = "/System/Volumes/Data/Users/dana/.local/share/broza/quarantine";

fn id(raw: &str) -> VolumeId {
    raw.parse().unwrap()
}

fn data_disk() -> Vec<Disk> {
    vec![Disk {
        id: id("disk0"),
        model: "SSD".into(),
        size_bytes: 1_000_000_000_000,
        internal: true,
        containers: vec![Container {
            id: id("disk3"),
            kind: FsKind::Apfs,
            size_bytes: 1_000_000_000_000,
            used_bytes: 500_000_000_000,
            free_bytes: 500_000_000_000,
            purgeable_bytes: 0,
            volumes: vec![Volume {
                id: id("disk3s5"),
                name: "Data".into(),
                uuid: None,
                role: VolumeRole::Data,
                mount_point: Some(PathBuf::from("/System/Volumes/Data")),
                used_bytes: 500_000_000_000,
                writable_by_broza: true,
                purpose: String::new(),
            }],
        }],
    }]
}

fn args() -> CleanArgs {
    CleanArgs {
        apply: false,
        categories: vec!["user-cache".into()],
        risk: None,
        purge: false,
        yes: false,
        max_size: None,
        exclude: Vec::new(),
        unused_after: None,
    }
}

fn world() -> (Ports, Handles) {
    let (ports, handles) = broza::testing::fake_ports();
    handles.disks.set_disks(data_disk());
    handles.fs.add_root("/", 1);
    handles.fs.add_root("/System/Volumes/Data", 2);
    handles.fs.add_file(CACHE_FILE, b"cached");
    handles.fs.set_size(CACHE_FILE, 900_000_000);
    handles
        .fs
        .add_file(format!("{HOME}/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw"), &[]);
    (ports, handles)
}

fn run_with(ports: &Ports, args: &CleanArgs, tty: bool, format: OutputFormat) -> Result<Outcome, BrozaError> {
    let config = Config::default();
    run(&CleanContext {
        ports,
        args,
        config: &config,
        host: Host { macos_version: "26.1".into(), arch: "arm64".into() },
        generated_at: "2026-09-21T10:36:08Z".parse().unwrap(),
        warnings: Vec::new(),
        policy: ColorPolicy::Never,
        format,
        folders: FolderSettings {
            home: Some(PathBuf::from(HOME)),
            cache_ttl: Duration::from_secs(60),
            no_cache: true,
            show_progress: false,
            verbose: false,
            own_stores: vec![PathBuf::from(STORE)],
        },
        tty,
        ci: false,
        uid_temp_dirs: Vec::new(),
    })
}

#[test]
fn a_dry_run_plans_the_caches_touches_nothing_and_exits_zero() {
    let (ports, handles) = world();

    let outcome = run_with(&ports, &args(), false, OutputFormat::Json).unwrap();

    let value: serde_json::Value = serde_json::from_str(&outcome.rendered).unwrap();
    assert_eq!(value["data"]["dry_run"], true);
    assert_eq!(value["data"]["items"][0]["status"], "planned");
    assert_eq!(value["data"]["quarantined_bytes"], 0);
    assert_eq!(outcome.code, ExitCode::Ok);
    assert!(handles.fs.exists(Path::new(CACHE_FILE)));
    assert!(handles.prompter.prompts().is_empty(), "a dry run never asks");
}

#[test]
fn a_selection_is_mandatory() {
    let (ports, _) = world();
    let none = CleanArgs { categories: Vec::new(), ..args() };

    let error = run_with(&ports, &none, false, OutputFormat::Human).expect_err("usage error");

    assert!(matches!(error, BrozaError::Usage(_)), "{error}");
    assert!(error.to_string().contains("--category"), "{error}");
}

#[test]
fn apply_without_a_terminal_or_yes_exits_seven() {
    let (ports, handles) = world();
    let apply = CleanArgs { apply: true, ..args() };

    let error = run_with(&ports, &apply, false, OutputFormat::Human).expect_err("no confirmation");

    assert_eq!(ExitCode::from(&error), ExitCode::ConfirmationRequired);
    assert!(handles.fs.exists(Path::new(CACHE_FILE)), "nothing moved");
}

#[test]
fn apply_with_yes_moves_the_caches_into_a_session_and_reports_pending_bytes() {
    let (ports, handles) = world();
    let apply = CleanArgs { apply: true, yes: true, ..args() };

    let outcome = run_with(&ports, &apply, false, OutputFormat::Json).unwrap();

    let value: serde_json::Value = serde_json::from_str(&outcome.rendered).unwrap();
    assert_eq!(value["data"]["dry_run"], false);
    assert_eq!(value["data"]["items"][0]["status"], "quarantined");
    assert!(value["data"]["quarantined_bytes"].as_u64().unwrap() > 0);
    assert_eq!(value["data"]["reclaimed_bytes"], 0, "quarantining frees nothing");
    assert_eq!(outcome.code, ExitCode::Ok);
    assert!(!handles.fs.exists(Path::new(CACHE_FILE)), "the cache left its place");
    let store = value["data"]["quarantine_path"].as_str().unwrap();
    assert!(store.starts_with(STORE), "{store}");
    assert!(handles.prompter.prompts().is_empty(), "--yes covers the confirmation");
}

#[test]
fn a_declined_confirmation_exits_six_and_moves_nothing() {
    let (ports, handles) = world();
    handles.prompter.queue(&[Answer::No]);
    let apply = CleanArgs { apply: true, ..args() };

    let error = run_with(&ports, &apply, true, OutputFormat::Human).expect_err("aborted");

    assert_eq!(ExitCode::from(&error), ExitCode::AbortedByUser);
    assert!(handles.fs.exists(Path::new(CACHE_FILE)));
}

#[test]
fn an_inform_only_finding_in_a_cleanable_category_is_skipped_with_a_warning() {
    let (ports, _) = world();
    let build = CleanArgs { categories: vec!["build-cache".into()], ..args() };

    let outcome = run_with(&ports, &build, false, OutputFormat::Human).unwrap();

    assert!(outcome.warnings.iter().any(|w| w.code == INFORM_ONLY_SKIPPED_CODE), "{:?}", outcome.warnings);
    assert_eq!(outcome.code, ExitCode::Ok);
}

#[test]
fn a_second_apply_expires_the_first_session_once_its_retention_is_over() {
    let (ports, handles) = world();
    let apply = CleanArgs { apply: true, yes: true, ..args() };
    run_with(&ports, &apply, false, OutputFormat::Json).unwrap();
    handles.fs.add_file(CACHE_FILE, b"cached again");
    handles.fs.set_size(CACHE_FILE, 900_000_000);
    handles.clock.advance(Config::default().quarantine_ttl.to_duration() + Duration::from_secs(86_400));

    let outcome = run_with(&ports, &apply, false, OutputFormat::Json).unwrap();

    let value: serde_json::Value = serde_json::from_str(&outcome.rendered).unwrap();
    assert_eq!(value["data"]["expired_sessions"].as_array().map(Vec::len), Some(1), "{}", outcome.rendered);
    assert!(value["data"]["reclaimed_bytes"].as_u64().unwrap() > 0, "expiry frees space");
    assert_eq!(outcome.code, ExitCode::Ok, "{:?}", outcome.warnings);
}
