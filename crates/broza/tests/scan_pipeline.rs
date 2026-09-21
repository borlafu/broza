//! `scan_volume`, `scan_all` and `scan_paths` end to end, over the fakes.
//!
//! The mount table is the one a stock Apple Silicon Mac reports
//! ([`mac_mount_table`]), so volume selection and the warnings are exercised
//! against real roles, devices and mount points. The cache is covered by
//! `scan_cache_pipeline.rs`.
#![cfg(feature = "test-support")]

mod scan_world;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use broza::BrozaError;
use broza::model::ItemKind;
use broza::scan::MountTable;
use broza::scan::{ScanProgress, ScanRequest, scan_all, scan_paths, scan_volume};
use broza::testing::mac_mount_table;
use scan_world::*;

#[test]
fn one_volume_is_walked_aggregated_and_reported() {
    let (ports, _handles) = ports();

    let scan = scan_data(&ports, &request());

    assert_eq!(scan.volume_id.as_str(), "disk3s5");
    assert_eq!(scan.root.path, Path::new(DATA));
    assert_eq!(scan.root.size_bytes, 8000);
    assert_eq!(scan.root.file_count, 3);
    assert!(scan.warnings.is_empty(), "{:?}", scan.warnings);
    assert!(
        item_paths(&scan).contains(&"/System/Volumes/Data/Users/dana/Movies/film.mov".to_owned()),
        "{:?}",
        item_paths(&scan)
    );
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
fn a_path_is_scanned_from_itself_down_on_the_volume_it_lives_on() {
    let (ports, _handles) = ports();
    let roots = vec![PathBuf::from("/System/Volumes/Data/Users/dana/Documents")];

    let scans = scan_paths(&roots, &request(), &ports, &mac_mount_table(), None)
        .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(scans.len(), 1);
    assert_eq!(scans[0].volume_id.as_str(), "disk3s5");
    assert_eq!(scans[0].root.path, roots[0]);
    assert_eq!(scans[0].root.size_bytes, 3000, "only what is under the path");
    assert_eq!(scans[0].tree.root.name, roots[0].display().to_string());
}

#[test]
fn a_path_that_does_not_exist_is_not_found() {
    let (ports, _handles) = ports();
    let missing = vec![PathBuf::from("/System/Volumes/Data/Users/dana/typo")];

    let error = scan_paths(&missing, &request(), &ports, &mac_mount_table(), None).err();

    assert!(matches!(error, Some(BrozaError::TargetNotFound(_))), "{error:?}");
}

#[test]
fn the_cloud_roots_are_excluded_in_the_data_volume_spelling_too() {
    let (ports, handles) = ports();
    let placeholder = "/System/Volumes/Data/Users/dana/Library/CloudStorage/Dropbox/huge.bin";
    handles.fs.add_file(placeholder, &[]);
    handles.fs.set_size(placeholder, 50_000);
    let request =
        ScanRequest { exclude: broza::scan::default_excludes(Path::new("/Users/dana")), ..request() };

    let scan = scan_data(&ports, &request);

    assert!(!item_paths(&scan).iter().any(|p| p.contains("CloudStorage")), "{:?}", item_paths(&scan));
    assert_eq!(scan.root.size_bytes, 8000, "the placeholder never entered the totals");
}

#[test]
fn an_excluded_folder_named_explicitly_is_still_scanned() {
    let (ports, handles) = ports();
    let cloud = "/System/Volumes/Data/Users/dana/Library/CloudStorage";
    handles.fs.add_file(format!("{cloud}/Dropbox/huge.bin"), &[]);
    handles.fs.set_size(format!("{cloud}/Dropbox/huge.bin"), 50_000);
    let request =
        ScanRequest { exclude: broza::scan::default_excludes(Path::new("/Users/dana")), ..request() };

    let scans = scan_paths(&[PathBuf::from(cloud)], &request, &ports, &mac_mount_table(), None)
        .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(scans[0].root.size_bytes, 50_000, "naming the folder is asking to see inside it");
}

#[test]
fn a_walk_that_measures_more_than_the_volume_holds_says_so() {
    let (ports, _handles) = ports();
    let mut entries = mac_mount_table().entries().to_vec();
    for entry in &mut entries {
        if entry.volume.id.as_str() == "disk3s5" {
            entry.volume.used_bytes = 1000;
        }
    }
    let mounts = MountTable::new(entries);

    let scan = scan_volume(&request().for_volume("disk3s5"), &ports, &mounts, None)
        .unwrap_or_else(|error| panic!("{error}"));

    assert!(scan.warnings.iter().any(|w| w.code == broza::scan::OVERCOUNT_CODE), "{:?}", scan.warnings);
}

#[test]
fn a_path_must_be_absolute_and_on_a_volume_broza_may_walk() {
    let (ports, _handles) = ports();
    let mounts = mac_mount_table();
    let scan = |root: &str| scan_paths(&[PathBuf::from(root)], &request(), &ports, &mounts, None).err();

    assert!(matches!(scan("Users/dana"), Some(BrozaError::Usage(_))));
    assert!(matches!(scan("/System/Library"), Some(BrozaError::TargetNotFound(_))), "sealed system volume");
    assert!(matches!(scan("/Volumes/Nowhere"), Some(BrozaError::TargetNotFound(_))));
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
fn a_directory_of_small_files_is_still_a_big_directory() {
    let (ports, handles) = ports();
    // Ten one-megabyte files: nothing in `pile` is reportable on its own, but
    // `pile` itself is, so its parent cannot be served from the cache.
    for index in 0..FILES_IN_THE_PILE {
        let path = format!("/System/Volumes/Data/Users/dana/parent/pile/f{index}");
        handles.fs.add_file(&path, &[]);
        handles.fs.set_size(&path, A_MEGABYTE);
    }
    let request = ScanRequest { min_size: 5 * A_MEGABYTE, depth: 1, ..request() };

    let cold = scan_data(&ports, &request);
    let warm = scan_data(&ports, &request);

    assert!(
        cold.largest.iter().any(|item| item.path.ends_with("pile")),
        "the pile is worth listing: {:?}",
        cold.largest
    );
    assert_eq!(warm, cold, "a warm scan may not lose the pile");
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
