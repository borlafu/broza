//! `scan_volume` and `scan_all` end to end, over the fakes.
//!
//! The mount table is the one a stock Apple Silicon Mac reports
//! ([`mac_mount_table`]), so volume selection, the cache layout and the warnings
//! are exercised against real roles, devices and mount points.
#![cfg(feature = "test-support")]

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use broza::model::ItemKind;
use broza::model::Volume;
use broza::ports::{FileOps, Ports};
use broza::scan::{MountEntry, MountTable};
use broza::scan::{ScanProgress, ScanRequest, VolumeScan, scan_all, scan_volume};
use broza::testing::{FakeFileOps, Handles, fake_ports, mac_mount_table};
use broza::{BrozaError, ExitCode};

/// Mount point of the Data volume in the fixture.
const DATA: &str = "/System/Volumes/Data";
/// Device of the Data volume in the fixture.
const DATA_DEVICE: u64 = 2;
/// Mount point of the external volume in the fixture.
const EXTERNAL: &str = "/Volumes/External";
/// Device of the external volume in the fixture.
const EXTERNAL_DEVICE: u64 = 6;
/// Where the CLI would put the cache.
const CACHE_ROOT: &str = "/Users/dana/.cache/broza";
/// Store of the Data volume inside that cache root.
const DATA_STORE: &str = "/Users/dana/.cache/broza/v1/22222222-2222-4222-8222-222222222222/dirs.bin";

/// A request that reports everything, with the cache under [`CACHE_ROOT`].
fn request() -> ScanRequest {
    ScanRequest {
        min_size: 0,
        top: 10,
        depth: 2,
        cache_root: Some(PathBuf::from(CACHE_ROOT)),
        cache_ttl: Duration::from_secs(3600),
        ..ScanRequest::default()
    }
}

/// Ports whose filesystem holds a Data volume with a few known files.
fn ports() -> (Ports, Handles) {
    let (ports, handles) = fake_ports();
    let fs: &FakeFileOps = handles.fs.as_ref();
    fs.add_root(DATA, DATA_DEVICE);
    for (path, size) in [
        ("/System/Volumes/Data/Users/dana/Movies/film.mov", 5000_u64),
        ("/System/Volumes/Data/Users/dana/Documents/notes.txt", 1000),
        ("/System/Volumes/Data/Users/dana/Documents/deep/archive.zip", 2000),
    ] {
        fs.add_file(path, &[]);
        fs.set_size(path, size);
    }
    (ports, handles)
}

fn scan_data(ports: &Ports, request: &ScanRequest) -> VolumeScan {
    scan_volume(&request.for_volume("disk3s5"), ports, &mac_mount_table(), None)
        .unwrap_or_else(|error| panic!("{error}"))
}

fn item_paths(scan: &VolumeScan) -> Vec<String> {
    scan.largest.iter().map(|item| item.path.display().to_string()).collect()
}

#[test]
fn one_volume_is_walked_aggregated_and_reported() {
    let (ports, _handles) = ports();

    let scan = scan_data(&ports, &request());

    assert_eq!(scan.volume_id.as_str(), "disk3s5");
    assert_eq!(scan.root.path, Path::new(DATA));
    assert_eq!(scan.root.size_bytes, 8000);
    assert_eq!(scan.root.file_count, 3);
    assert!(scan.warnings.is_empty(), "{:?}", scan.warnings);
    assert!(item_paths(&scan).contains(&"/System/Volumes/Data/Users/dana/Movies".to_owned()));
    assert!(scan.largest.iter().any(|item| item.kind == ItemKind::File));
    assert_eq!(scan.largest.iter().map(|item| item.volume_id.as_str()).next(), Some("disk3s5"));
}

#[test]
fn the_tree_view_stops_at_the_requested_depth() {
    let (ports, _handles) = ports();

    let scan = scan_data(&ports, &ScanRequest { depth: 1, ..request() });

    let root = scan.tree.root;
    assert_eq!(root.name, DATA);
    assert_eq!(root.children.iter().map(|child| child.name.clone()).collect::<Vec<_>>(), vec!["Users"]);
    assert!(root.children[0].children.is_empty(), "depth 1 stops below the first level");
}

#[test]
fn scanning_one_volume_needs_that_volume_to_exist_and_to_be_writable() {
    let (ports, _handles) = ports();
    let mounts = mac_mount_table();

    let no_volume = scan_volume(&request(), &ports, &mounts, None).err();
    let sealed = scan_volume(&request().for_volume("disk3s1"), &ports, &mounts, None).err();
    let unknown = scan_volume(&request().for_volume("disk9s9"), &ports, &mounts, None).err();

    assert!(matches!(no_volume, Some(BrozaError::Usage(_))), "{no_volume:?}");
    assert!(matches!(sealed, Some(BrozaError::TargetNotFound(_))), "{sealed:?}");
    assert!(matches!(unknown, Some(BrozaError::TargetNotFound(_))), "{unknown:?}");
}

#[test]
fn a_warm_scan_reports_exactly_what_the_cold_one_did() {
    let (ports, handles) = ports();
    // With this threshold, `Documents` holds nothing that could be listed on
    // its own, so a warm scan may take it from the cache; `Movies` holds a file
    // that could be, so it is walked again.
    let request = ScanRequest { min_size: 2500, depth: 1, ..request() };

    let cold = scan_data(&ports, &request);
    let warm = scan_data(&ports, &request);

    assert_eq!(warm, cold, "a warm scan is the same scan, only faster");
    assert!(!warm.largest.is_empty());
    assert_eq!(warm.root.file_count, cold.root.file_count);
    assert_eq!(warm.tree, cold.tree);
    assert!(handles.fs.exists(Path::new(DATA_STORE)));
}

#[test]
fn a_subtree_the_cache_answered_for_is_not_walked_again() {
    let (ports, handles) = ports();
    let request = ScanRequest { min_size: 2500, depth: 1, ..request() };

    let cold = scan_data(&ports, &request);
    // The file grows in place: its directory's mtime does not change, which is
    // the staleness the TTL bounds (`docs/implementation-plan.md` §3.4).
    handles.fs.set_size("/System/Volumes/Data/Users/dana/Documents/notes.txt", 1500);
    let warm = scan_data(&ports, &request);
    let forced = scan_data(&ports, &ScanRequest { no_cache: true, ..request });

    assert_eq!(warm.root.size_bytes, cold.root.size_bytes, "the cached subtree was reused");
    assert_eq!(forced.root.size_bytes, cold.root.size_bytes + 500, "--no-cache measures again");
}

#[test]
fn a_subtree_holding_something_reportable_is_never_taken_from_the_cache() {
    let (ports, handles) = ports();
    let request = ScanRequest { min_size: 2500, depth: 1, ..request() };

    let cold = scan_data(&ports, &request);
    // `film.mov` is above the threshold, so `Movies` must be walked again and
    // the growth must show up.
    handles.fs.set_size("/System/Volumes/Data/Users/dana/Movies/film.mov", 6000);
    let warm = scan_data(&ports, &request);

    assert_eq!(warm.root.size_bytes, cold.root.size_bytes + 1000);
}

#[test]
fn the_cache_is_written_under_the_volume_of_the_layout_version() {
    let (ports, handles) = ports();

    let _ = scan_data(&ports, &request());

    assert!(handles.fs.exists(Path::new(DATA_STORE)), "{:?}", handles.fs.paths());
}

#[test]
fn no_cache_skips_reading_the_store_but_still_writes_one() {
    let (ports, handles) = ports();

    let _ = scan_data(&ports, &ScanRequest { no_cache: true, ..request() });

    assert!(handles.fs.exists(Path::new(DATA_STORE)));
}

#[test]
fn a_scan_without_a_cache_root_writes_nothing_and_still_reports() {
    let (ports, handles) = ports();
    let before = handles.fs.paths();

    let scan = scan_data(&ports, &ScanRequest { cache_root: None, ..request() });

    assert_eq!(scan.root.size_bytes, 8000);
    assert_eq!(handles.fs.paths(), before, "no cache root, no file written");
}

#[test]
fn a_volume_with_no_uuid_says_its_cache_is_filed_under_a_disk_slot() {
    let (ports, _handles) = ports();
    let mounts = MountTable::new(
        mac_mount_table()
            .entries()
            .iter()
            .cloned()
            .map(|entry| MountEntry { volume: Volume { uuid: None, ..entry.volume }, ..entry })
            .collect(),
    );

    let scan = scan_volume(&request().for_volume("disk3s5"), &ports, &mounts, None)
        .unwrap_or_else(|e| panic!("{e}"));

    let codes: Vec<&str> = scan.warnings.iter().map(|warning| warning.code.as_str()).collect();
    assert_eq!(codes, vec!["cache_keyed_by_bsd_id"], "{:?}", scan.warnings);
}

#[test]
fn a_volume_with_a_uuid_files_its_cache_under_it() {
    let (ports, handles) = ports();

    let scan = scan_data(&ports, &request());

    assert!(handles.fs.exists(Path::new(DATA_STORE)), "{:?}", handles.fs.paths());
    assert!(scan.warnings.is_empty(), "{:?}", scan.warnings);
}

#[test]
fn scanning_one_volume_reports_progress_when_asked_to() {
    let (ports, _handles) = ports();
    let seen: Mutex<Vec<ScanProgress>> = Mutex::new(Vec::new());
    let sink = |progress: ScanProgress| {
        seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(progress);
    };

    let scan = scan_volume(&request().for_volume("disk3s5"), &ports, &mac_mount_table(), Some(&sink))
        .unwrap_or_else(|e| panic!("{e}"));

    let seen = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let last = seen.last().unwrap_or_else(|| panic!("no progress at all"));
    assert_eq!(last.bytes_scanned, scan.root.size_bytes);
}

#[test]
fn a_corrupt_cache_stops_the_scan_with_exit_nine() {
    let (ports, handles) = ports();
    handles.fs.add_file(DATA_STORE, b"BRZC\x07nonsense");

    let error = scan_volume(&request().for_volume("disk3s5"), &ports, &mac_mount_table(), None).err();

    let Some(error @ BrozaError::Cache(_)) = error else { panic!("{error:?}") };
    assert_eq!(ExitCode::from(&error), ExitCode::CacheError);
    assert!(error.to_string().contains("--no-cache"), "{error}");
}

#[test]
fn an_unreadable_subtree_becomes_a_warning_and_the_volume_is_still_reported() {
    let (ports, handles) = ports();
    handles.fs.add_denied("/System/Volumes/Data/Users/dana/Documents");

    let scan = scan_data(&ports, &request());

    assert_eq!(scan.root.size_bytes, 5000);
    assert_eq!(scan.warnings.len(), 1, "{:?}", scan.warnings);
    assert_eq!(scan.warnings[0].code, "permission_denied");
}

#[test]
fn every_writable_volume_is_scanned_in_volume_order() {
    let (ports, handles) = ports();
    handles.fs.add_root(EXTERNAL, EXTERNAL_DEVICE);
    handles.fs.add_file("/Volumes/External/backup.dmg", &[]);
    handles.fs.set_size("/Volumes/External/backup.dmg", 700);

    let scans = scan_all(&request(), &ports, &mac_mount_table(), None).unwrap_or_else(|e| panic!("{e}"));

    let ids: Vec<&str> = scans.iter().map(|scan| scan.volume_id.as_str()).collect();
    assert_eq!(ids, vec!["disk3s5", "disk4s1"], "sealed volumes are never walked");
    assert_eq!(scans[1].root.size_bytes, 700);
}

#[test]
fn no_external_leaves_the_disks_under_volumes_out() {
    let (ports, handles) = ports();
    handles.fs.add_root(EXTERNAL, EXTERNAL_DEVICE);

    let request = ScanRequest { include_external: false, ..request() };
    let scans = scan_all(&request, &ports, &mac_mount_table(), None).unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(scans.iter().map(|scan| scan.volume_id.as_str()).collect::<Vec<_>>(), vec!["disk3s5"]);
}

#[test]
fn progress_is_reported_while_everything_is_scanned() {
    let (ports, _handles) = ports();
    let seen: Mutex<Vec<ScanProgress>> = Mutex::new(Vec::new());
    let sink = |progress: ScanProgress| {
        seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(progress);
    };

    let scans =
        scan_all(&request(), &ports, &mac_mount_table(), Some(&sink)).unwrap_or_else(|e| panic!("{e}"));

    let seen = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let last = seen.last().unwrap_or_else(|| panic!("no progress at all"));
    assert_eq!(last.bytes_scanned, scans[0].root.size_bytes);
    assert!(last.entries_scanned >= 3, "{last:?}");
}

#[test]
fn a_volume_whose_root_cannot_be_read_is_reported_as_empty_and_warned_about() {
    let (ports, handles) = ports();
    handles.fs.add_denied(DATA);

    let scan = scan_data(&ports, &request());

    assert_eq!(scan.root.path, Path::new(DATA));
    assert_eq!(scan.root.size_bytes, 0);
    assert!(scan.root.children_truncated);
    assert_eq!(scan.warnings.len(), 1, "{:?}", scan.warnings);
}
