//! Tests of the folder walk's settings and results.
#![allow(clippy::unwrap_used)]

use std::path::Path;

use broza::scan::default_excludes;
use broza::testing::mac_mount_table;

use super::*;

fn args() -> ScanArgs {
    ScanArgs {
        paths: Vec::new(),
        depth: 2,
        top: 20,
        min_size: None,
        volume: None,
        no_external: false,
        tree: false,
        interactive: false,
    }
}

/// A volume scan with nothing in it, for tests about the flags around it.
fn sample_scan() -> VolumeScan {
    let root = broza::scan::DirNode {
        path: PathBuf::from("/System/Volumes/Data"),
        size_bytes: 0,
        allocated_bytes: 0,
        file_count: 0,
        dir_count: 0,
        dataless_count: 0,
        largest_item_bytes: 0,
        has_hard_links: false,
        has_truncation: true,
        device: 2,
        inode: 1,
        mtime: None,
        children_truncated: true,
        from_cache: false,
    };
    VolumeScan {
        volume_id: "disk3s5".parse().unwrap(),
        root,
        largest: Vec::new(),
        tree: TreeView {
            root: broza::scan::TreeNode {
                name: "Data".into(),
                path: PathBuf::from("/System/Volumes/Data"),
                size_bytes: 0,
                percent_of_parent: 100.0,
                other_bytes: 0,
                children: Vec::new(),
            },
        },
        warnings: Vec::new(),
        nodes: Vec::new(),
        files: Vec::new(),
        clone_families: Vec::new(),
        root_refused: false,
    }
}

fn settings() -> FolderSettings {
    FolderSettings {
        home: Some(PathBuf::from("/Users/dana")),
        cache_ttl: Duration::from_secs(60),
        no_cache: false,
        show_progress: false,
        verbose: false,
        own_stores: vec![PathBuf::from("/Users/dana/.local/share/broza/quarantine")],
    }
}

#[test]
fn brozas_own_stores_are_excluded_in_both_spellings() {
    let request = request_for(&args(), &settings()).unwrap();

    assert!(request.exclude.contains(&PathBuf::from("/Users/dana/.local/share/broza/quarantine")));
    assert!(
        request
            .exclude
            .contains(&PathBuf::from("/System/Volumes/Data/Users/dana/.local/share/broza/quarantine"))
    );
}

#[test]
fn the_merged_list_is_capped_at_top_across_volumes() {
    use broza::model::{ItemKind, LargestItem};
    use broza::scan::{DirNode, TreeNode, TreeView};
    let item = |path: &str, size: u64| LargestItem {
        path: PathBuf::from(path),
        size_bytes: size,
        kind: ItemKind::File,
        volume_id: "disk3s5".parse().unwrap(),
    };
    let node = DirNode {
        path: PathBuf::from("/"),
        size_bytes: 0,
        allocated_bytes: 0,
        file_count: 0,
        dir_count: 0,
        dataless_count: 0,
        largest_item_bytes: 0,
        has_hard_links: false,
        has_truncation: false,
        device: 1,
        inode: 1,
        mtime: None,
        children_truncated: false,
        from_cache: false,
    };
    let tree = TreeView {
        root: TreeNode {
            name: "/".into(),
            path: PathBuf::from("/"),
            size_bytes: 0,
            percent_of_parent: 100.0,
            other_bytes: 0,
            children: Vec::new(),
        },
    };
    let scan = |items: Vec<LargestItem>| VolumeScan {
        volume_id: "disk3s5".parse().unwrap(),
        root: node.clone(),
        largest: items,
        tree: tree.clone(),
        warnings: Vec::new(),
        nodes: Vec::new(),
        files: Vec::new(),
        clone_families: Vec::new(),
        root_refused: false,
    };
    let scans = vec![scan(vec![item("/a", 5), item("/b", 4)]), scan(vec![item("/c", 3), item("/d", 2)])];

    let results = results_of(scans, &mac_mount_table(), 3);

    assert_eq!(results.largest.len(), 3);
    assert_eq!(results.largest[0].path, PathBuf::from("/a"));
    assert_eq!(results.largest[2].path, PathBuf::from("/c"));
}

#[test]
fn the_request_carries_the_flags_the_home_and_the_cache() {
    let request = request_for(&args(), &settings()).unwrap();

    assert_eq!(request.min_size, 100_000_000, "the scan default, not the config key");
    assert_eq!(request.depth, 2);
    assert_eq!(request.top, 20);
    assert!(request.include_external);
    for cloud in default_excludes(Path::new("/Users/dana")) {
        assert!(request.exclude.contains(&cloud), "{cloud:?}");
    }
    assert_eq!(request.cache_root, Some(PathBuf::from("/Users/dana/.cache/broza")));
    assert_eq!(request.cache_ttl, Duration::from_secs(60));
    assert!(request.volume.is_none(), "selection happened at disk level already");
}

#[test]
fn an_explicit_min_size_is_parsed_and_a_bad_one_is_a_usage_error() {
    let good = request_for(&ScanArgs { min_size: Some("2GB".into()), ..args() }, &settings()).unwrap();
    let bad = request_for(&ScanArgs { min_size: Some("lots".into()), ..args() }, &settings()).err();

    assert_eq!(good.min_size, 2_000_000_000);
    assert!(matches!(bad, Some(BrozaError::Usage(_))), "{bad:?}");
}

#[test]
fn only_a_named_root_macos_refused_ends_the_scan() {
    let refused = VolumeScan { root_refused: true, ..sample_scan() };

    let named = refused_named_root(vec![refused.clone()], true);
    let whole_volume = refused_named_root(vec![refused], false);

    assert!(matches!(named, Err(BrozaError::PermissionDenied { .. })), "{named:?}");
    assert!(whole_volume.is_ok(), "an unreadable mount point is a warning, not an error");
}

#[test]
fn without_a_home_there_is_no_cache_and_nothing_excluded() {
    let settings = FolderSettings { home: None, own_stores: Vec::new(), ..settings() };
    let request = request_for(&args(), &settings).unwrap();

    assert!(request.cache_root.is_none());
    assert!(request.exclude.is_empty(), "{:?}", request.exclude);
}

#[test]
fn only_the_selected_volumes_reach_the_walker() {
    let mounts = mac_mount_table();
    let data_only: Vec<Disk> = vec![];

    let none = mounts_of(&data_only, &mounts);

    assert!(none.entries().is_empty(), "no disks selected, nothing walked");
}
