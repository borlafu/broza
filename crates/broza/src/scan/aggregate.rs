//! Turning a walk into what the user reads: largest items, tree, usage bar.
//!
//! Everything here is a pure function of a [`WalkResult`]: no I/O, no clock, no
//! configuration. Formatting bytes stays in the CLI (`AGENTS.md` §6).
//!
//! # Which items are "largest"
//!
//! A flat "biggest directories" list is useless on a real disk: `~/Library`,
//! `~/Library/Developer`, `~/Library/Developer/Xcode` and its `DerivedData` are
//! four entries for the same bytes. Broza applies one rule, and only one:
//!
//! > **A directory that is reported hides its descendants, unless a descendant is
//! > at least half of the nearest reported ancestor.**
//!
//! So `~/Library` is reported once; `~/Library/Developer` also appears when it
//! carries at least half of `~/Library`, because then the parent's line alone
//! would hide where the space actually is. The scanned root itself is never
//! reported: it is the volume, and it would hide everything.

pub mod bar;
pub mod tree;

use std::path::Path;

pub use bar::usage_bar;
pub use tree::{TreeNode, TreeView, tree};

use crate::model::{ItemKind, LargestItem, VolumeId};
use crate::scan::walker::WalkResult;

/// Smallest share of its nearest reported ancestor an entry must carry to be
/// reported as well, expressed as the divisor of that ancestor's size.
const DOMINANT_SHARE_DIVISOR: u64 = 2;

/// One entry competing for a place in the list, borrowed from the walk.
#[derive(Debug, Clone, Copy)]
struct Candidate<'a> {
    /// Absolute path.
    path: &'a Path,
    /// Apparent size in bytes.
    size_bytes: u64,
    /// File or directory.
    kind: ItemKind,
}

/// The `top` largest items of a walk, biggest first.
///
/// Directories and files compete in the same list; the suppression rule of the
/// module documentation decides which descendants survive. Items smaller than
/// `min_size` never appear, and neither do cloud placeholders: the walk does not
/// collect them, because none of their bytes are on this disk. Ties are broken
/// by path, so two runs of the same scan produce the same list.
///
/// Nothing is copied until the list is decided: the candidates borrow from the
/// walk, and only the survivors — at most `top` of them — become owned items.
pub fn largest_items(walk: &WalkResult, top: usize, min_size: u64, volume_id: &VolumeId) -> Vec<LargestItem> {
    let root = walk.root().map(|node| node.path.as_path());
    ranked_candidates(walk, min_size, root)
        .into_iter()
        .fold(Vec::new(), |accepted, candidate| accept(accepted, candidate, top))
        .into_iter()
        .map(|candidate| LargestItem {
            path: candidate.path.to_path_buf(),
            size_bytes: candidate.size_bytes,
            kind: candidate.kind,
            volume_id: volume_id.clone(),
        })
        .collect()
}

/// Every item above `min_size`, largest first and then by path.
fn ranked_candidates<'a>(walk: &'a WalkResult, min_size: u64, root: Option<&Path>) -> Vec<Candidate<'a>> {
    let dirs = walk.nodes.iter().filter(|node| Some(node.path.as_path()) != root).map(|node| Candidate {
        path: node.path.as_path(),
        size_bytes: node.size_bytes,
        kind: ItemKind::Directory,
    });
    let files = walk.files.iter().map(|file| Candidate {
        path: file.path.as_path(),
        size_bytes: file.size_bytes,
        kind: ItemKind::File,
    });
    let mut candidates: Vec<Candidate<'a>> =
        dirs.chain(files).filter(|candidate| candidate.size_bytes >= min_size).collect();
    candidates.sort_by(|left, right| right.size_bytes.cmp(&left.size_bytes).then(left.path.cmp(right.path)));
    candidates
}

/// Add `candidate` to the list when the suppression rule allows it.
fn accept<'a>(accepted: Vec<Candidate<'a>>, candidate: Candidate<'a>, top: usize) -> Vec<Candidate<'a>> {
    if accepted.len() >= top || !is_worth_reporting(&accepted, &candidate) {
        return accepted;
    }
    let mut accepted = accepted;
    accepted.push(candidate);
    accepted
}

/// `true` when no reported ancestor already speaks for this entry.
fn is_worth_reporting(accepted: &[Candidate<'_>], candidate: &Candidate<'_>) -> bool {
    nearest_ancestor(accepted, candidate.path)
        .is_none_or(|ancestor| candidate.size_bytes >= ancestor.size_bytes / DOMINANT_SHARE_DIVISOR)
}

/// The deepest already reported directory that contains `path`.
fn nearest_ancestor<'a, 'b>(accepted: &'a [Candidate<'b>], path: &Path) -> Option<&'a Candidate<'b>> {
    accepted
        .iter()
        .filter(|other| other.kind == ItemKind::Directory && path.starts_with(other.path))
        .filter(|other| other.path != path)
        .max_by_key(|other| other.path.components().count())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::largest_items;
    use crate::model::{ItemKind, VolumeId};
    use crate::scan::walker::{DirNode, FileEntry, WalkResult};

    fn dir(path: &str, size_bytes: u64) -> DirNode {
        DirNode {
            path: PathBuf::from(path),
            size_bytes,
            allocated_bytes: size_bytes,
            file_count: 1,
            dir_count: 0,
            dataless_count: 0,
            largest_item_bytes: size_bytes,
            device: 1,
            inode: 7,
            mtime: None,
            children_truncated: false,
        }
    }

    fn file(path: &str, size_bytes: u64) -> FileEntry {
        FileEntry { path: PathBuf::from(path), size_bytes, allocated_bytes: size_bytes }
    }

    fn volume() -> VolumeId {
        "disk3s5".parse().unwrap_or_else(|e| panic!("{e}"))
    }

    fn result(nodes: Vec<DirNode>, files: Vec<FileEntry>) -> WalkResult {
        WalkResult { nodes, files, errors: Vec::new() }
    }

    fn reported(walk: &WalkResult, top: usize, min_size: u64) -> Vec<(String, u64, ItemKind)> {
        largest_items(walk, top, min_size, &volume())
            .into_iter()
            .map(|item| (item.path.display().to_string(), item.size_bytes, item.kind))
            .collect()
    }

    #[test]
    fn directories_and_files_are_mixed_and_sorted_largest_first() {
        let walk = result(
            vec![dir("/vol", 1000), dir("/vol/a", 300), dir("/vol/b", 200)],
            vec![file("/vol/big.iso", 500)],
        );

        assert_eq!(
            reported(&walk, 10, 0),
            vec![
                ("/vol/big.iso".to_owned(), 500, ItemKind::File),
                ("/vol/a".to_owned(), 300, ItemKind::Directory),
                ("/vol/b".to_owned(), 200, ItemKind::Directory),
            ]
        );
    }

    #[test]
    fn the_root_is_never_reported_because_it_is_the_whole_volume() {
        let walk = result(vec![dir("/vol", 1000), dir("/vol/a", 300)], Vec::new());

        let paths: Vec<_> = reported(&walk, 10, 0).into_iter().map(|item| item.0).collect();

        assert_eq!(paths, vec!["/vol/a".to_owned()]);
    }

    #[test]
    fn a_reported_directory_hides_the_descendants_it_dwarfs() {
        let walk = result(
            vec![dir("/vol", 1000), dir("/vol/a", 900), dir("/vol/a/small", 100), dir("/vol/b", 80)],
            Vec::new(),
        );

        let paths: Vec<_> = reported(&walk, 10, 0).into_iter().map(|item| item.0).collect();

        assert_eq!(paths, vec!["/vol/a".to_owned(), "/vol/b".to_owned()]);
    }

    #[test]
    fn a_descendant_that_is_half_of_its_parent_is_worth_reporting_too() {
        let walk = result(
            vec![
                dir("/vol", 1000),
                dir("/vol/a", 900),
                dir("/vol/a/most", 450),
                dir("/vol/a/most/some", 200),
            ],
            Vec::new(),
        );

        let paths: Vec<_> = reported(&walk, 10, 0).into_iter().map(|item| item.0).collect();

        // `most` is exactly half of `a`, so it is reported; `some` is under half
        // of `most`, its nearest reported ancestor, so it is not.
        assert_eq!(paths, vec!["/vol/a".to_owned(), "/vol/a/most".to_owned()]);
    }

    #[test]
    fn a_chain_of_dominant_directories_is_reported_all_the_way_down() {
        let walk = result(
            vec![dir("/vol", 1000), dir("/vol/a", 900), dir("/vol/a/most", 890), dir("/vol/a/most/all", 880)],
            Vec::new(),
        );

        let paths: Vec<_> = reported(&walk, 10, 0).into_iter().map(|item| item.0).collect();

        assert_eq!(
            paths,
            vec!["/vol/a".to_owned(), "/vol/a/most".to_owned(), "/vol/a/most/all".to_owned()],
            "when the space is all in one deep folder, saying so is the point"
        );
    }

    #[test]
    fn a_file_inside_a_reported_directory_follows_the_same_rule() {
        let walk = result(
            vec![dir("/vol", 1000), dir("/vol/a", 900)],
            vec![file("/vol/a/huge.bin", 800), file("/vol/a/tiny.bin", 20)],
        );

        let paths: Vec<_> = reported(&walk, 10, 0).into_iter().map(|item| item.0).collect();

        assert_eq!(paths, vec!["/vol/a".to_owned(), "/vol/a/huge.bin".to_owned()]);
    }

    #[test]
    fn items_below_the_minimum_size_are_dropped() {
        let walk =
            result(vec![dir("/vol", 1000), dir("/vol/a", 300), dir("/vol/b", 99)], vec![file("/vol/f", 50)]);

        let paths: Vec<_> = reported(&walk, 10, 100).into_iter().map(|item| item.0).collect();

        assert_eq!(paths, vec!["/vol/a".to_owned()]);
    }

    #[test]
    fn only_the_requested_number_of_items_comes_back() {
        let walk = result(
            vec![dir("/vol", 1000), dir("/vol/a", 300), dir("/vol/b", 200), dir("/vol/c", 100)],
            Vec::new(),
        );

        assert_eq!(reported(&walk, 2, 0).len(), 2);
        assert!(reported(&walk, 0, 0).is_empty());
    }

    #[test]
    fn items_of_the_same_size_come_back_in_path_order() {
        let walk = result(vec![dir("/vol", 10), dir("/vol/b", 5), dir("/vol/a", 5)], Vec::new());

        let paths: Vec<_> = reported(&walk, 10, 0).into_iter().map(|item| item.0).collect();

        assert_eq!(paths, vec!["/vol/a".to_owned(), "/vol/b".to_owned()]);
    }

    #[test]
    fn every_item_carries_the_volume_it_was_found_on() {
        let walk = result(vec![dir("/vol", 10), dir("/vol/a", 5)], Vec::new());

        let items = largest_items(&walk, 5, 0, &volume());

        assert_eq!(items[0].volume_id, volume());
    }

    #[test]
    fn a_walk_without_nodes_reports_nothing() {
        assert!(largest_items(&WalkResult::default(), 5, 0, &volume()).is_empty());
    }
}
