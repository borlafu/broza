//! The scan cache end to end: what a warm scan may reuse, what it must walk
//! again, and how the store is written, keyed and expired.
#![cfg(feature = "test-support")]

mod scan_world;

use std::path::{Path, PathBuf};
use std::time::Duration;

use broza::model::Volume;
use broza::ports::FileOps;
use broza::scan::{FileReport, ScanRequest, scan_paths, scan_volume};
use broza::scan::{MountEntry, MountTable};
use broza::testing::mac_mount_table;
use broza::{BrozaError, ExitCode};
use scan_world::*;

#[test]
fn the_cache_keeps_records_measured_after_the_store_was_opened() {
    // A real clock ticks between opening the store and stamping the records;
    // those records are newer than the store, not stale.
    let (ports, handles) = ports();
    let request = request();

    let _cold = scan_data(&ports, &request);
    handles.clock.advance(A_MINUTE);
    let _warm = scan_data(&ports, &request);

    let store = handles.fs.read(Path::new(DATA_STORE)).unwrap_or_else(|error| panic!("{error}"));
    assert!(store.len() > 64, "the store holds records, not just a header: {} bytes", store.len());
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
    // Thresholds are allocated bytes (4 KiB units in the fake): at 5000 only
    // `Movies` holds something listable, so `Documents` may come from the cache.
    let request = ScanRequest { min_size: 5000, depth: 1, ..request() };

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
fn a_cached_subtree_expires_on_the_ttl_it_was_first_recorded_with() {
    let (ports, handles) = ports();
    // `Documents` holds nothing large enough to be listed, so a warm scan
    // takes it from the cache — which must not renew it.
    let request = ScanRequest { min_size: 5000, depth: 1, cache_ttl: AN_HOUR, ..request() };

    let cold = scan_data(&ports, &request);
    handles.fs.set_size("/System/Volumes/Data/Users/dana/Documents/notes.txt", 1500);
    // Half an hour later, and half an hour after that: both inside the hour.
    handles.clock.advance(HALF_AN_HOUR);
    let warm = scan_data(&ports, &request);
    handles.clock.advance(HALF_AN_HOUR);
    handles.clock.advance(A_MINUTE);
    let expired = scan_data(&ports, &request);

    assert_eq!(warm.root.size_bytes, cold.root.size_bytes, "inside the hour, the cache answers");
    assert_eq!(
        expired.root.size_bytes,
        cold.root.size_bytes + 500,
        "past the hour the subtree is measured again: a served record must not be re-stamped, \
         or a stale subtree would be served for ever"
    );
}

#[test]
fn hard_links_across_a_cached_boundary_are_still_counted_once() {
    let (ports, handles) = ports();
    // Forty names of one file, spread over two directories: the walk settles
    // them by seeing every name at once, so a subtree holding any of them can
    // never be served from the cache.
    let original = "/System/Volumes/Data/Users/dana/links/original.bin";
    handles.fs.add_file(original, &[]);
    handles.fs.set_size(original, A_MEGABYTE);
    for index in 0..LINKS_IN_THE_PROBE {
        handles.fs.add_hard_link(original, format!("/System/Volumes/Data/Users/dana/copies/n{index}"));
    }
    let request = ScanRequest { min_size: 5 * A_MEGABYTE, depth: 1, ..request() };

    let cold = scan_data(&ports, &request);
    let warm = scan_data(&ports, &request);

    assert_eq!(warm.root.size_bytes, cold.root.size_bytes, "the links were counted twice");
    assert_eq!(warm, cold);
}

#[test]
fn a_subtree_holding_every_name_of_its_files_is_still_served_from_the_cache() {
    let (ports, handles) = ports();
    // Forty names of one file, all of them in `copies`: nobody outside will
    // count that file, so skipping the subtree loses nothing.
    let original = "/System/Volumes/Data/Users/dana/copies/original.bin";
    handles.fs.add_file(original, &[]);
    handles.fs.set_size(original, 1000);
    for index in 0..LINKS_IN_THE_PROBE {
        handles.fs.add_hard_link(original, format!("/System/Volumes/Data/Users/dana/copies/n{index}"));
    }
    handles.fs.add_file("/System/Volumes/Data/Users/dana/copies/extra.bin", &[]);
    handles.fs.set_size("/System/Volumes/Data/Users/dana/copies/extra.bin", 300);
    // Allocated threshold: 4 KiB files stay below 5000, so `copies` is cacheable.
    let request = ScanRequest { min_size: 5000, depth: 1, ..request() };

    let cold = scan_data(&ports, &request);
    // Growing a file leaves the directory's mtime alone, so a cached subtree
    // reports the old number — which is how this test knows it was cached.
    handles.fs.set_size("/System/Volumes/Data/Users/dana/copies/extra.bin", 900);
    let warm = scan_data(&ports, &request);

    assert_eq!(cold.root.size_bytes, 8000 + 1000 + 300, "the forty names are one file");
    assert_eq!(warm.root.size_bytes, cold.root.size_bytes, "`copies` came from the cache");
    assert_eq!(warm, cold);
}

#[test]
fn a_subtree_with_a_hole_in_it_is_measured_again_and_warned_about_again() {
    let (ports, handles) = ports();
    // Everything in `quiet` is too small to be listed, so the cache would
    // happily answer for it — but one directory *below* it cannot be read, and
    // the warning that says so only exists while somebody is walking.
    handles.fs.add_file("/System/Volumes/Data/Users/dana/quiet/sub/secret/hidden.bin", &[]);
    handles.fs.set_size("/System/Volumes/Data/Users/dana/quiet/sub/secret/hidden.bin", 100);
    handles.fs.add_denied("/System/Volumes/Data/Users/dana/quiet/sub/secret");
    let request = ScanRequest { min_size: 2500, depth: 1, ..request() };

    let cold = scan_data(&ports, &request);
    let warm = scan_data(&ports, &request);

    assert!(!cold.warnings.is_empty(), "the unreadable directory has to be reported");
    assert_eq!(warm.warnings, cold.warnings, "a warm scan may not lose the reason");
    assert_eq!(warm, cold);
}

#[test]
fn a_warm_scan_that_measured_nothing_new_leaves_the_store_alone() {
    let (ports, handles) = ports();
    let request = ScanRequest { min_size: 2500, depth: 1, ..request() };

    let _ = scan_data(&ports, &request);
    let written = handles.fs.read(Path::new(DATA_STORE)).unwrap_or_else(|e| panic!("{e}"));
    let _ = scan_data(&ports, &request);

    assert_eq!(
        handles.fs.read(Path::new(DATA_STORE)).unwrap_or_else(|e| panic!("{e}")),
        written,
        "nothing changed, so the file should not have been rewritten"
    );
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
fn two_roots_on_one_volume_share_one_store_and_both_are_kept() {
    let (ports, handles) = ports();
    let alpha = PathBuf::from("/System/Volumes/Data/Users/dana/Movies");
    let beta = PathBuf::from("/System/Volumes/Data/Users/dana/Documents");
    let request = request();

    let _both = scan_paths(&[alpha.clone(), beta.clone()], &request, &ports, &mac_mount_table(), None)
        .unwrap_or_else(|error| panic!("{error}"));
    let store_after_both = handles.fs.read(Path::new(DATA_STORE)).unwrap_or_else(|error| panic!("{error}"));

    // Scanning each root alone must add nothing the joint store did not already hold.
    let _alpha =
        scan_paths(&[alpha], &request, &ports, &mac_mount_table(), None).unwrap_or_else(|e| panic!("{e}"));
    let _beta =
        scan_paths(&[beta], &request, &ports, &mac_mount_table(), None).unwrap_or_else(|e| panic!("{e}"));
    let store_after_each = handles.fs.read(Path::new(DATA_STORE)).unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(store_after_both, store_after_each, "the joint scan already held both roots' records");
}

/// The request `suggest` makes: every directory node and every file from 1 MB.
fn for_detectors() -> ScanRequest {
    let report = FileReport::for_detectors();
    ScanRequest { file_report: Some(report), min_size: report.min_size, depth: 0, top: 0, ..request() }
}

const BIG: &str = "/System/Volumes/Data/Users/dana/Documents/big.bin";

#[test]
fn a_warm_walk_for_the_detectors_serves_whole_subtrees_with_their_files() {
    let (ports, handles) = ports();
    handles.fs.add_file(BIG, &[]);
    handles.fs.set_size(BIG, 5 * A_MEGABYTE);

    let cold = scan_data(&ports, &for_detectors());
    let warm = scan_data(&ports, &for_detectors());

    let paths =
        |scan: &broza::scan::VolumeScan| scan.nodes.iter().map(|n| n.path.clone()).collect::<Vec<_>>();
    assert_eq!(paths(&warm), paths(&cold), "every directory node is back");
    assert_eq!(warm.files, cold.files, "every file above the floor is back");
    assert!(warm.files.iter().any(|file| file.path == Path::new(BIG)), "{:?}", warm.files);
    let documents = warm
        .nodes
        .iter()
        .find(|node| node.path == Path::new("/System/Volumes/Data/Users/dana/Documents"))
        .unwrap_or_else(|| panic!("Documents"));
    assert!(documents.from_cache, "Documents was served, not walked");
    assert!(!warm.root.from_cache, "the root is always walked");
}

#[test]
fn a_served_subtree_keeps_the_sizes_it_was_recorded_with_until_the_cache_is_bypassed() {
    let (ports, handles) = ports();
    handles.fs.add_file(BIG, &[]);
    handles.fs.set_size(BIG, 5 * A_MEGABYTE);
    let cold = scan_data(&ports, &for_detectors());

    // Grown in place: the directory's mtime is unchanged, so the cache answers.
    handles.fs.set_size(BIG, 6 * A_MEGABYTE);
    let warm = scan_data(&ports, &for_detectors());
    let forced = scan_data(&ports, &ScanRequest { no_cache: true, ..for_detectors() });

    let big = |scan: &broza::scan::VolumeScan| {
        scan.files.iter().find(|file| file.path == Path::new(BIG)).map(|file| file.size_bytes)
    };
    assert_eq!(big(&warm), big(&cold), "the cached file entry is the recorded one");
    assert_eq!(big(&forced), Some(6 * A_MEGABYTE), "--no-cache measures again");
}

#[test]
fn a_store_written_by_an_older_broza_is_replaced_without_an_error() {
    let (ports, handles) = ports();
    handles.fs.add_dir(Path::new(DATA_STORE).parent().unwrap_or_else(|| panic!("parent")));
    handles.fs.add_file(DATA_STORE, b"BRZC\x01whatever the old layout held");

    let scan = scan_data(&ports, &request());

    assert!(!scan.nodes.is_empty(), "the scan ran");
    let bytes = handles.fs.read(Path::new(DATA_STORE)).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(bytes[4], broza::scan::cache::STORE_VERSION, "rewritten in the current layout");
}

#[test]
fn a_walk_that_does_not_serve_from_the_cache_reads_the_disk_and_keeps_the_store() {
    let (ports, handles) = ports();
    handles.fs.add_file(BIG, &[]);
    handles.fs.set_size(BIG, 5 * A_MEGABYTE);
    let cold = scan_data(&ports, &for_detectors());
    let outside = "/System/Volumes/Data/Users/dana/Movies";
    let outside_key = cold
        .nodes
        .iter()
        .find(|node| node.path == Path::new(outside))
        .and_then(|node| broza::scan::CacheKey::of(&node.identity()))
        .unwrap_or_else(|| panic!("a key for {outside}"));

    // What `clean` does: the home walked cold, the store loaded and refreshed.
    handles.fs.set_size(BIG, 6 * A_MEGABYTE);
    let home = PathBuf::from("/System/Volumes/Data/Users/dana/Documents");
    let request = ScanRequest { serve_from_cache: false, ..for_detectors() };
    let mut scans =
        scan_paths(&[home], &request, &ports, &mac_mount_table(), None).unwrap_or_else(|e| panic!("{e}"));
    let cold_again = scans.pop().unwrap_or_else(|| panic!("a scan"));

    let big = cold_again.files.iter().find(|file| file.path == Path::new(BIG)).map(|file| file.size_bytes);
    assert_eq!(big, Some(6 * A_MEGABYTE), "nothing came from the cache");
    let store = broza::scan::CacheStore::load(
        Path::new(DATA_STORE),
        handles.fs.as_ref(),
        handles.clock.as_ref(),
        Duration::from_secs(3600),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(store.lookup(&outside_key).is_some(), "the records of the rest of the volume survived the save");
}
