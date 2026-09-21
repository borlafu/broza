//! Counting a hard-linked file once, and always in the same place.
//!
//! One file with three names is one file's worth of disk (`docs/cli-spec.md`
//! §4.2). Deciding *while* walking which name to count means the answer depends
//! on which thread got there first, and two runs of the same scan then disagree
//! about which directory holds the bytes. So the walk counts every name and
//! records the sightings; this pass gives the bytes to the name whose path
//! sorts first and takes them back out of the other names' directories.
//!
//! The same pass decides which directories may be cached. A hard link only
//! stops a subtree from being served when one of its names is *outside* it: a
//! directory holding all forty names of one file is self-contained, and a
//! later scan that skips it still counts that file once, because nobody else
//! will count it. So a directory is marked `has_hard_links` when it holds some
//! but not all names of an inode, or when the walk did not see every name at
//! all (`link_count` says there are more).
//!
//! `largest_item_bytes` is recomputed after settlement from the surviving
//! names ([`Settled::credited`]) and each directory's single-name files, so a
//! discounted link never poses as a directory's biggest item. One thing is left
//! slightly generous: a name created *outside* a self-contained subtree after
//! that subtree was recorded is counted twice
//! until the record expires: the walk sees the new name and an inode that
//! claims more names than it can find, while the cached total already counted
//! the file. It is bounded by `cache-ttl`, like every other thing a cache can
//! be wrong about, and it errs towards reporting more space in use than there
//! is (`docs/cli-spec.md` §4.2).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::scan::walker::parts::LinkSighting;
use crate::scan::walker::{DirNode, FileEntry};

/// Settle every hard link: count it once, and mark who may not be cached.
pub(super) fn settle_hard_links(
    nodes: Vec<DirNode>,
    files: Vec<FileEntry>,
    links: Vec<LinkSighting>,
) -> Settled {
    if links.is_empty() {
        return Settled { nodes, files, credited: Vec::new() };
    }
    let nodes = mark_uncacheable(nodes, &hiders(&links));
    let duplicates = duplicates_of(links.clone());
    let dropped: HashSet<&Path> = duplicates.iter().map(|link| link.path.as_path()).collect();
    let credited = links.into_iter().filter(|link| !dropped.contains(link.path.as_path())).collect();
    if duplicates.is_empty() {
        return Settled { nodes, files, credited };
    }
    let kept_files = files.into_iter().filter(|file| !dropped.contains(file.path.as_path())).collect();
    Settled { nodes: subtract(nodes, &duplicates), files: kept_files, credited }
}

/// What settling the hard links leaves behind.
pub(super) struct Settled {
    /// The nodes, with duplicate names discounted and hiders marked.
    pub nodes: Vec<DirNode>,
    /// The collected files, minus the discounted names.
    pub files: Vec<FileEntry>,
    /// The one sighting per inode that keeps the bytes: the name whose
    /// directory the file is credited to. What the largest item of that
    /// directory is recomputed from.
    pub credited: Vec<LinkSighting>,
}

/// Directories that hold some, but not all, of an inode's names.
///
/// For an inode whose names were all seen, those are the directories between
/// each name and the lowest directory that contains every name: anything above
/// that holds the whole set and is free to be cached. For an inode with names
/// the walk never saw — another volume, an excluded path, a subtree that was
/// itself served from the cache — no directory can be sure it holds them all.
fn hiders(links: &[LinkSighting]) -> HashSet<PathBuf> {
    let mut hiders = HashSet::new();
    for (_, group) in group_by_inode(links) {
        let complete = u64::try_from(group.len())
            .is_ok_and(|seen| Some(seen) == group.first().map(|link| link.link_count));
        let whole_set = complete.then(|| lowest_common_directory(&group)).flatten();
        for link in &group {
            for ancestor in link.path.ancestors().skip(1) {
                if whole_set.as_deref() == Some(ancestor) {
                    break;
                }
                hiders.insert(ancestor.to_path_buf());
            }
        }
    }
    hiders
}

/// The sightings of each inode, grouped.
fn group_by_inode(links: &[LinkSighting]) -> HashMap<(u64, u64), Vec<&LinkSighting>> {
    links.iter().fold(HashMap::new(), |mut groups, link| {
        groups.entry((link.device, link.inode)).or_insert_with(Vec::new).push(link);
        groups
    })
}

/// The deepest directory that contains every one of these names.
fn lowest_common_directory(group: &[&LinkSighting]) -> Option<PathBuf> {
    let mut common: PathBuf = group.first()?.path.parent()?.to_path_buf();
    for link in group.iter().skip(1) {
        let parent = link.path.parent()?;
        while !parent.starts_with(&common) {
            if !common.pop() {
                return None;
            }
        }
    }
    Some(common)
}

/// Mark the directories that may not be served from the cache.
fn mark_uncacheable(nodes: Vec<DirNode>, hiders: &HashSet<PathBuf>) -> Vec<DirNode> {
    nodes
        .into_iter()
        .map(|node| if hiders.contains(&node.path) { node.hiding_a_name() } else { node })
        .collect()
}

/// Every sighting that is not the first name of its inode, by path.
fn duplicates_of(links: Vec<LinkSighting>) -> Vec<LinkSighting> {
    let mut links = links;
    links.sort_by(|left, right| {
        (left.device, left.inode, &left.path).cmp(&(right.device, right.inode, &right.path))
    });
    let mut keeper: Option<(u64, u64)> = None;
    links
        .into_iter()
        .filter(|link| {
            let inode = (link.device, link.inode);
            if keeper == Some(inode) {
                return true;
            }
            keeper = Some(inode);
            false
        })
        .collect()
}

/// Remove each duplicate's bytes from its directory and every directory above.
fn subtract(nodes: Vec<DirNode>, duplicates: &[LinkSighting]) -> Vec<DirNode> {
    let index: HashMap<PathBuf, usize> =
        nodes.iter().enumerate().map(|(at, node)| (node.path.clone(), at)).collect();
    let mut nodes = nodes;
    for duplicate in duplicates {
        for ancestor in duplicate.path.ancestors().skip(1) {
            let Some(node) = index.get(ancestor).and_then(|at| nodes.get_mut(*at)) else { continue };
            node.size_bytes = node.size_bytes.saturating_sub(duplicate.size_bytes);
            node.allocated_bytes = node.allocated_bytes.saturating_sub(duplicate.allocated_bytes);
            node.file_count = node.file_count.saturating_sub(1);
        }
    }
    nodes
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{FileEntry, LinkSighting, settle_hard_links};
    use crate::scan::walker::DirNode;

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

    /// A sighting of one of `link_count` names of `inode`.
    fn named(path: &str, inode: u64, size_bytes: u64, link_count: u64) -> LinkSighting {
        LinkSighting {
            device: 1,
            inode,
            path: PathBuf::from(path),
            link_count,
            size_bytes,
            allocated_bytes: size_bytes,
        }
    }

    /// A sighting of one of two names, which is what most probes want.
    fn sighting(path: &str, inode: u64, size_bytes: u64) -> LinkSighting {
        named(path, inode, size_bytes, 2)
    }

    /// Paths of the nodes that may not be served from the cache.
    fn hidden(nodes: &[DirNode]) -> Vec<String> {
        let mut paths: Vec<String> = nodes
            .iter()
            .filter(|node| node.has_hard_links)
            .map(|node| node.path.display().to_string())
            .collect();
        paths.sort();
        paths
    }

    fn file(path: &str, size_bytes: u64) -> FileEntry {
        FileEntry { path: PathBuf::from(path), size_bytes, allocated_bytes: size_bytes }
    }

    fn sizes(nodes: &[DirNode]) -> Vec<(String, u64, u64)> {
        nodes.iter().map(|node| (node.path.display().to_string(), node.size_bytes, node.file_count)).collect()
    }

    #[test]
    fn a_walk_without_hard_links_is_left_alone() {
        let nodes = vec![node("/vol", 100, 1)];

        let settled = settle_hard_links(nodes, vec![file("/vol/f", 100)], Vec::new());
        let (nodes, files) = (settled.nodes, settled.files);

        assert_eq!(sizes(&nodes), vec![("/vol".to_owned(), 100, 1)]);
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn the_first_path_keeps_the_bytes_whatever_order_the_sightings_arrive_in() {
        let nodes = vec![node("/vol", 200, 2), node("/vol/a", 100, 1), node("/vol/b", 100, 1)];
        let forwards = vec![sighting("/vol/a/f", 7, 100), sighting("/vol/b/f", 7, 100)];
        let backwards = vec![sighting("/vol/b/f", 7, 100), sighting("/vol/a/f", 7, 100)];

        let first = settle_hard_links(nodes.clone(), Vec::new(), forwards).nodes;
        let second = settle_hard_links(nodes, Vec::new(), backwards).nodes;

        assert_eq!(sizes(&first), sizes(&second));
        assert_eq!(
            sizes(&first),
            vec![("/vol".to_owned(), 100, 1), ("/vol/a".to_owned(), 100, 1), ("/vol/b".to_owned(), 0, 0),]
        );
    }

    #[test]
    fn a_duplicate_is_taken_out_of_the_reported_files_too() {
        let nodes = vec![node("/vol", 200, 2)];
        let links = vec![sighting("/vol/a", 7, 100), sighting("/vol/b", 7, 100)];

        let files = settle_hard_links(nodes, vec![file("/vol/a", 100), file("/vol/b", 100)], links).files;

        assert_eq!(
            files.iter().map(|file| file.path.display().to_string()).collect::<Vec<_>>(),
            vec!["/vol/a".to_owned()]
        );
    }

    #[test]
    fn two_names_of_two_different_inodes_both_keep_their_bytes() {
        let nodes = vec![node("/vol", 300, 3)];
        let links = vec![sighting("/vol/a", 7, 100), sighting("/vol/b", 8, 100)];

        let nodes = settle_hard_links(nodes, Vec::new(), links).nodes;

        assert_eq!(sizes(&nodes), vec![("/vol".to_owned(), 300, 3)]);
    }

    #[test]
    fn three_names_of_one_inode_leave_one_copy_behind() {
        let nodes = vec![node("/vol", 300, 3)];
        let links = vec![named("/vol/c", 7, 100, 3), named("/vol/a", 7, 100, 3), named("/vol/b", 7, 100, 3)];

        let nodes = settle_hard_links(nodes, Vec::new(), links).nodes;

        assert_eq!(sizes(&nodes), vec![("/vol".to_owned(), 100, 1)]);
    }

    #[test]
    fn a_directory_holding_every_name_of_a_file_may_still_be_cached() {
        let nodes = vec![node("/vol", 200, 2), node("/vol/copies", 200, 2)];
        // Both names live in `copies`, and the filesystem says there are two.
        let links = vec![named("/vol/copies/a", 7, 100, 2), named("/vol/copies/b", 7, 100, 2)];

        let nodes = settle_hard_links(nodes, Vec::new(), links).nodes;

        assert!(hidden(&nodes).is_empty(), "nobody hides a name: {:?}", hidden(&nodes));
    }

    #[test]
    fn a_directory_holding_one_name_of_two_may_not_be_cached() {
        let nodes = vec![node("/vol", 200, 2), node("/vol/a", 100, 1), node("/vol/b", 100, 1)];
        let links = vec![named("/vol/a/f", 7, 100, 2), named("/vol/b/f", 7, 100, 2)];

        let nodes = settle_hard_links(nodes, Vec::new(), links).nodes;

        // `a` and `b` each hold one name of two; `vol` holds both, so it is
        // free to be cached.
        assert_eq!(hidden(&nodes), vec!["/vol/a".to_owned(), "/vol/b".to_owned()]);
    }

    #[test]
    fn a_name_the_walk_never_saw_makes_every_directory_above_uncacheable() {
        let nodes = vec![node("/vol", 100, 1), node("/vol/here", 100, 1)];
        // Two names exist; only one is inside the walk.
        let links = vec![named("/vol/here/f", 7, 100, 2)];

        let nodes = settle_hard_links(nodes, Vec::new(), links).nodes;

        assert_eq!(hidden(&nodes), vec!["/vol".to_owned(), "/vol/here".to_owned()]);
    }

    #[test]
    fn a_subtraction_never_wraps_below_zero() {
        let nodes = vec![node("/vol", 10, 0)];
        let links = vec![sighting("/vol/a", 7, 100), sighting("/vol/b", 7, 100)];

        let nodes = settle_hard_links(nodes, Vec::new(), links).nodes;

        assert_eq!(sizes(&nodes), vec![("/vol".to_owned(), 0, 0)]);
    }
}
