//! What the clone settlement must do, family by family.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{CloneLedger, Originals, settle_clones};
use crate::ports::EntryMetadata;
use crate::scan::walker::{DirNode, FileEntry};

fn node(path: &str, size_bytes: u64, file_count: u64) -> DirNode {
    DirNode {
        path: PathBuf::from(path),
        size_bytes,
        allocated_bytes: size_bytes,
        file_count,
        dir_count: 0,
        dataless_count: 0,
        largest_item_bytes: size_bytes,
        has_hard_links: false,
        has_truncation: false,
        device: 1,
        inode: 1,
        mtime: None,
        children_truncated: false,
        from_cache: false,
    }
}

/// Metadata of a clone with `inode`, of the family whose original is `family`.
fn clone_meta(inode: u64, family: u64, size_bytes: u64) -> EntryMetadata {
    EntryMetadata {
        device: 1,
        inode,
        size_bytes,
        allocated_bytes: size_bytes,
        link_count: 1,
        is_dir: false,
        is_symlink: false,
        is_dataless: false,
        modified: None,
        accessed: None,
        clone_id: Some(family),
    }
}

/// A ledger with one clone recorded per `(path, inode, family, size)`.
fn ledger(clones: &[(&str, u64, u64, u64)]) -> CloneLedger {
    clones.iter().fold(CloneLedger::default(), |mut ledger, (path, inode, family, size)| {
        let path = PathBuf::from(path);
        let dir: Arc<Path> = Arc::from(path.parent().unwrap_or(Path::new("/")));
        ledger.record(&dir, path, &clone_meta(*inode, *family, *size));
        ledger
    })
}

fn originals(inodes: &[u64]) -> Originals {
    inodes.iter().fold(Originals::default(), |mut set, inode| {
        set.record(1, *inode);
        set
    })
}

fn sizes(nodes: &[DirNode]) -> Vec<(String, u64, u64)> {
    nodes.iter().map(|node| (node.path.display().to_string(), node.size_bytes, node.file_count)).collect()
}

fn file(path: &str, inode: u64, family: u64, size_bytes: u64) -> FileEntry {
    FileEntry { inode, clone_id: Some(family), device: 1, ..FileEntry::sized(path, size_bytes) }
}

#[test]
fn every_clone_of_an_original_the_walk_saw_is_discounted() {
    let nodes = vec![node("/vol", 300, 3), node("/vol/copies", 200, 2)];
    let ledger = ledger(&[("/vol/copies/a", 11, 7, 100), ("/vol/copies/b", 12, 7, 100)]);

    let result = settle_clones(nodes, Vec::new(), Vec::new(), ledger, originals(&[7]));

    assert_eq!(sizes(&result.nodes), vec![("/vol".to_owned(), 100, 1), ("/vol/copies".to_owned(), 0, 0)]);
    assert!(result.credited.is_empty(), "the original holds the bytes, as an ordinary file");
}

#[test]
fn without_the_original_the_first_path_keeps_the_bytes() {
    let nodes = vec![node("/vol", 200, 2), node("/vol/a", 100, 1), node("/vol/b", 100, 1)];
    // Recorded in the wrong order on purpose.
    let ledger = ledger(&[("/vol/b/f", 12, 7, 100), ("/vol/a/f", 11, 7, 100)]);

    let result = settle_clones(nodes, Vec::new(), Vec::new(), ledger, originals(&[]));

    assert_eq!(
        sizes(&result.nodes),
        vec![("/vol".to_owned(), 100, 1), ("/vol/a".to_owned(), 100, 1), ("/vol/b".to_owned(), 0, 0)]
    );
    assert_eq!(result.credited.len(), 1);
    assert_eq!(result.credited[0].path, PathBuf::from("/vol/a/f"));
}

#[test]
fn two_ledgers_merge_into_the_same_answer_whatever_the_split() {
    let nodes = vec![node("/vol", 400, 4)];
    let left = ledger(&[("/vol/d", 14, 8, 100), ("/vol/a", 11, 7, 100)]);
    let right = ledger(&[("/vol/b", 12, 7, 100), ("/vol/c", 13, 8, 100)]);

    let result = settle_clones(nodes, Vec::new(), Vec::new(), left.merge(right), originals(&[]));

    assert_eq!(sizes(&result.nodes), vec![("/vol".to_owned(), 200, 2)]);
    let mut kept: Vec<String> = result.credited.iter().map(|c| c.path.display().to_string()).collect();
    kept.sort();
    assert_eq!(kept, vec!["/vol/a".to_owned(), "/vol/c".to_owned()]);
}

#[test]
fn a_kept_family_is_recorded_under_its_keeper_s_directory_and_stays_cacheable() {
    let nodes = vec![node("/vol", 300, 3), node("/vol/a", 100, 1), node("/vol/b", 200, 2)];
    let ledger = ledger(&[("/vol/a/f", 11, 7, 100), ("/vol/b/f", 12, 7, 100), ("/vol/b/g", 13, 8, 100)]);

    let result = settle_clones(nodes, Vec::new(), Vec::new(), ledger, originals(&[8]));

    assert!(
        result.nodes.iter().all(|node| !node.has_hard_links),
        "clones never make a directory uncacheable"
    );
    assert_eq!(result.kept_clones, vec![(PathBuf::from("/vol/a"), (1, 7))], "family 8 has its original");
}

#[test]
fn a_family_kept_in_a_cached_subtree_reads_as_one_with_an_original() {
    // What the walker does with a served record's `kept_clones`: the
    // family is handed over as an original, and the walked clone is
    // discounted.
    let nodes = vec![node("/vol", 100, 1), node("/vol/b", 100, 1)];
    let ledger = ledger(&[("/vol/b/f", 12, 7, 100)]);

    let result = settle_clones(nodes, Vec::new(), Vec::new(), ledger, originals(&[7]));

    assert_eq!(sizes(&result.nodes), vec![("/vol".to_owned(), 0, 0), ("/vol/b".to_owned(), 0, 0)]);
    assert!(result.kept_clones.is_empty());
}

#[test]
fn a_discounted_file_frees_nothing_and_a_keeper_or_an_original_keeps_its_bytes() {
    let nodes = vec![node("/vol", 400, 4)];
    let ledger = ledger(&[("/vol/a", 11, 7, 100), ("/vol/b", 12, 7, 100), ("/vol/c", 13, 8, 100)]);
    let files = vec![
        file("/vol/a", 11, 7, 100),
        file("/vol/b", 12, 7, 100),
        file("/vol/c", 13, 8, 100),
        file("/vol/o", 8, 8, 100),
    ];

    let result = settle_clones(nodes, files.clone(), files, ledger, originals(&[8]));

    let sizes: Vec<(&str, u64)> =
        result.files.iter().map(|file| (file.path.to_str().unwrap_or(""), file.allocated_bytes)).collect();
    assert_eq!(sizes, vec![("/vol/a", 100), ("/vol/b", 0), ("/vol/c", 0), ("/vol/o", 100)]);
    assert_eq!(result.cache_files.len(), 4, "the cache's list is treated alike");
    assert_eq!(result.cache_files[2].allocated_bytes, 0);
}

#[test]
fn a_walk_without_clones_is_left_alone() {
    let nodes = vec![node("/vol", 100, 1)];

    let result =
        settle_clones(nodes.clone(), Vec::new(), Vec::new(), CloneLedger::default(), originals(&[1, 2]));

    assert_eq!(result.nodes, nodes);
}
