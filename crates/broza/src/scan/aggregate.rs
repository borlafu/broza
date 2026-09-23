//! Turning a walk into what the user reads: largest items, tree, usage bar.
//!
//! Everything here is a pure function of a [`WalkResult`]: no I/O, no clock, no
//! configuration. Formatting — bytes, percentages, usage bars — stays in the
//! CLI (`AGENTS.md` §6).
//!
//! # Which items are "largest"
//!
//! A flat "biggest directories" list is useless on a real disk: `~/Library`,
//! `~/Library/Developer`, `~/Library/Developer/Xcode` and its `DerivedData` are
//! four entries for the same bytes. Broza drills down instead of listing chains:
//!
//! > **A directory is reported when no single child holds at least half of it.
//! > When one child does, Broza descends into that child, and every other child
//! > above `--min-size` is judged on its own by the same rule.**
//!
//! So a home folder whose bytes sit in `~/Library/Developer/Xcode/DerivedData`
//! is reported as that one `DerivedData` line (its many projects share the
//! space, so nothing below dominates), while `~/Library/Caches` and
//! `~/Documents` — smaller siblings on the way down — still get their own
//! lines. A file is a leaf and is reported as itself. The scanned root is never
//! reported: it is the volume, and it would hide everything.
//!
//! Dominance is read from the walker's `largest_item_bytes`, which every
//! directory carries whether it was walked this run or served from the cache,
//! so a warm scan lists exactly what a cold one does.

pub mod tree;

use std::collections::HashMap;
use std::path::Path;

pub use tree::{TreeNode, TreeView, tree};

use crate::model::{ItemKind, LargestItem, VolumeId};
use crate::scan::walker::WalkResult;

/// Divisor of a directory's size a child must reach to be "dominant" (one half).
const DOMINANT_SHARE_DIVISOR: u64 = 2;

/// One entry competing for a place in the list, borrowed from the walk.
#[derive(Debug, Clone, Copy)]
struct Candidate<'a> {
    /// Absolute path.
    path: &'a Path,
    /// Allocated size in bytes: what the report prints and what every rule
    /// here compares, because it is what deleting the item would free
    /// (`AGENTS.md` §2.7). Apparent lengths never enter this module.
    allocated_bytes: u64,
    /// File or directory.
    kind: ItemKind,
    /// Largest single item inside a directory, in allocated bytes (`0` for a
    /// file). Equal to the largest direct child, so it decides dominance even
    /// when the children themselves are not in the walk — a subtree served
    /// from the cache.
    largest_inside: u64,
}

/// Every reported directory and collected file, indexed by parent directory.
struct Children<'a> {
    by_parent: HashMap<&'a Path, Vec<Candidate<'a>>>,
}

impl<'a> Children<'a> {
    fn of(walk: &'a WalkResult) -> Self {
        let mut by_parent: HashMap<&'a Path, Vec<Candidate<'a>>> = HashMap::new();
        let dirs = walk.nodes.iter().map(|node| Candidate {
            path: node.path.as_path(),
            allocated_bytes: node.allocated_bytes,
            kind: ItemKind::Directory,
            largest_inside: node.largest_item_bytes,
        });
        let files = walk.files.iter().map(|file| Candidate {
            path: file.path.as_path(),
            allocated_bytes: file.allocated_bytes,
            kind: ItemKind::File,
            largest_inside: 0,
        });
        for candidate in dirs.chain(files) {
            if let Some(parent) = candidate.path.parent() {
                by_parent.entry(parent).or_default().push(candidate);
            }
        }
        Self { by_parent }
    }

    fn under(&self, path: &Path) -> &[Candidate<'a>] {
        self.by_parent.get(path).map_or(&[], Vec::as_slice)
    }
}

/// The `top` largest items of a walk, biggest first.
///
/// Expects an unlimited walk (`max_depth: None`, as `scan_volume` always asks):
/// with directories hidden by a depth limit, a dominated directory has nothing
/// visible to drill into and its bytes would vanish from the list.
///
/// Sizes are **allocated** bytes — blocks on the disk — because that is what
/// removing the item gives back. Apparent lengths can exceed the volume (sparse
/// files, APFS clones) and would make the list lie about what is freeable.
///
/// Directories and files compete in the same list; the drill-down rule of the
/// module documentation decides which directories speak for their contents.
/// Items smaller than `min_size` never appear, and neither do cloud
/// placeholders: the walk does not collect them, because none of their bytes
/// are on this disk. Ties are broken by path, so two runs of the same scan
/// produce the same list.
pub fn largest_items(walk: &WalkResult, top: usize, min_size: u64, volume_id: &VolumeId) -> Vec<LargestItem> {
    let Some(root) = walk.root() else { return Vec::new() };
    let children = Children::of(walk);
    let mut reported: Vec<Candidate<'_>> = Vec::new();
    for child in children.under(&root.path) {
        report(*child, &children, min_size, &mut reported);
    }
    reported.sort_by(|left, right| {
        right.allocated_bytes.cmp(&left.allocated_bytes).then(left.path.cmp(right.path))
    });
    reported
        .into_iter()
        .take(top)
        .map(|candidate| LargestItem {
            path: candidate.path.to_path_buf(),
            size_bytes: candidate.allocated_bytes,
            kind: candidate.kind,
            volume_id: volume_id.clone(),
        })
        .collect()
}

/// Report `entry` itself, or drill into it when one child dominates it.
fn report<'a>(entry: Candidate<'a>, children: &Children<'a>, min_size: u64, out: &mut Vec<Candidate<'a>>) {
    if entry.allocated_bytes < min_size {
        return;
    }
    if entry.kind == ItemKind::File {
        out.push(entry);
        return;
    }
    if !is_dominated(&entry) {
        out.push(entry);
        return;
    }
    // The dominant child may be a file too small to have been collected, or
    // the whole subtree may have come from the cache: either way, whatever is
    // visible below is judged on its own and the rest is too small to list.
    for child in children.under(entry.path) {
        report(*child, children, min_size, out);
    }
}

/// `true` when one item inside `entry` alone holds at least half of it.
///
/// The walker records the largest item of every directory from *every* entry it
/// saw, collected or not, so this answer is the same for a directory walked
/// this run and for one served from the cache.
fn is_dominated(entry: &Candidate<'_>) -> bool {
    entry.allocated_bytes > 0
        && entry.largest_inside.saturating_mul(DOMINANT_SHARE_DIVISOR) >= entry.allocated_bytes
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::largest_items;
    use crate::model::{ItemKind, VolumeId};
    use crate::scan::walker::{DirNode, FileEntry, WalkResult};

    /// A directory whose biggest item is `largest`: the walker's
    /// `largest_item_bytes`, which the drill-down rule reads.
    fn dir_holding(path: &str, size_bytes: u64, largest: u64) -> DirNode {
        DirNode { largest_item_bytes: largest, ..dir(path, size_bytes) }
    }

    /// A directory of many small things: nothing inside dominates it.
    fn dir(path: &str, size_bytes: u64) -> DirNode {
        DirNode {
            path: PathBuf::from(path),
            size_bytes,
            allocated_bytes: size_bytes,
            file_count: 1,
            dir_count: 0,
            dataless_count: 0,
            largest_item_bytes: size_bytes / 4,
            has_hard_links: false,
            has_truncation: false,
            device: 1,
            inode: 7,
            mtime: None,
            children_truncated: false,
            from_cache: false,
        }
    }

    fn file(path: &str, size_bytes: u64) -> FileEntry {
        FileEntry::sized(path, size_bytes)
    }

    fn volume() -> VolumeId {
        "disk3s5".parse().unwrap_or_else(|e| panic!("{e}"))
    }

    fn result(nodes: Vec<DirNode>, files: Vec<FileEntry>) -> WalkResult {
        WalkResult { nodes, files, ..WalkResult::default() }
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
    fn a_directory_nothing_dominates_is_reported_and_hides_its_contents() {
        let walk = result(
            vec![
                dir("/vol", 1000),
                dir_holding("/vol/a", 900, 400),
                dir("/vol/a/x", 400),
                dir("/vol/a/y", 400),
                dir("/vol/b", 80),
            ],
            Vec::new(),
        );

        let paths: Vec<_> = reported(&walk, 10, 0).into_iter().map(|item| item.0).collect();

        assert_eq!(paths, vec!["/vol/a".to_owned(), "/vol/b".to_owned()]);
    }

    #[test]
    fn a_dominating_child_is_drilled_into_and_its_siblings_are_judged_on_their_own() {
        let walk = result(
            vec![
                dir("/vol", 1000),
                dir_holding("/vol/a", 900, 700),
                dir_holding("/vol/a/most", 700, 300),
                dir("/vol/a/most/p", 300),
                dir("/vol/a/most/q", 300),
                dir("/vol/a/rest", 150),
            ],
            Vec::new(),
        );

        let paths: Vec<_> = reported(&walk, 10, 0).into_iter().map(|item| item.0).collect();

        // `a` is dominated by `most`, so `a` itself is not a line: `most` is
        // (nothing under it dominates), and the sibling `rest` gets its own line.
        assert_eq!(paths, vec!["/vol/a/most".to_owned(), "/vol/a/rest".to_owned()]);
    }

    #[test]
    fn a_chain_of_dominant_directories_collapses_to_where_the_bytes_are() {
        let walk = result(
            vec![
                dir("/vol", 1000),
                dir_holding("/vol/a", 900, 890),
                dir_holding("/vol/a/most", 890, 880),
                dir_holding("/vol/a/most/all", 880, 870),
            ],
            vec![file("/vol/a/most/all/blob.bin", 870)],
        );

        let paths: Vec<_> = reported(&walk, 10, 0).into_iter().map(|item| item.0).collect();

        assert_eq!(paths, vec!["/vol/a/most/all/blob.bin".to_owned()], "one line, not four, for one file");
    }

    #[test]
    fn a_file_that_dominates_its_directory_is_the_line_and_a_small_sibling_is_not() {
        let walk = result(
            vec![dir("/vol", 1000), dir_holding("/vol/a", 900, 800)],
            vec![file("/vol/a/huge.bin", 800), file("/vol/a/tiny.bin", 20)],
        );

        let paths: Vec<_> = reported(&walk, 10, 100).into_iter().map(|item| item.0).collect();

        assert_eq!(paths, vec!["/vol/a/huge.bin".to_owned()]);
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
