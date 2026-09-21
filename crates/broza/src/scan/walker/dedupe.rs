//! Counting a hard-linked file once, and always in the same place.
//!
//! One file with three names is one file's worth of disk (`docs/cli-spec.md`
//! §4.2). Deciding *while* walking which name to count means the answer depends
//! on which thread got there first, and two runs of the same scan then disagree
//! about which directory holds the bytes. So the walk counts every name and
//! records the sightings; this pass gives the bytes to the name whose path
//! sorts first and takes them back out of the other names' directories.
//!
//! What is left slightly generous is `largest_item_bytes`: a subtree whose only
//! big file turned out to be a discounted link still claims it. That only makes
//! the cache descend into a subtree it could have skipped, never the reverse.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::scan::walker::parts::LinkSighting;
use crate::scan::walker::{DirNode, FileEntry};

/// Take the duplicate names of hard-linked files back out of the totals.
pub(super) fn discount_duplicates(
    nodes: Vec<DirNode>,
    files: Vec<FileEntry>,
    links: Vec<LinkSighting>,
) -> (Vec<DirNode>, Vec<FileEntry>) {
    let duplicates = duplicates_of(links);
    if duplicates.is_empty() {
        return (nodes, files);
    }
    let dropped: HashSet<&Path> = duplicates.iter().map(|link| link.path.as_path()).collect();
    let kept_files = files.into_iter().filter(|file| !dropped.contains(file.path.as_path())).collect();
    (subtract(nodes, &duplicates), kept_files)
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

    use super::{FileEntry, LinkSighting, discount_duplicates};
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

    fn sighting(path: &str, inode: u64, size_bytes: u64) -> LinkSighting {
        LinkSighting { device: 1, inode, path: PathBuf::from(path), size_bytes, allocated_bytes: size_bytes }
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

        let (nodes, files) = discount_duplicates(nodes, vec![file("/vol/f", 100)], Vec::new());

        assert_eq!(sizes(&nodes), vec![("/vol".to_owned(), 100, 1)]);
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn the_first_path_keeps_the_bytes_whatever_order_the_sightings_arrive_in() {
        let nodes = vec![node("/vol", 200, 2), node("/vol/a", 100, 1), node("/vol/b", 100, 1)];
        let forwards = vec![sighting("/vol/a/f", 7, 100), sighting("/vol/b/f", 7, 100)];
        let backwards = vec![sighting("/vol/b/f", 7, 100), sighting("/vol/a/f", 7, 100)];

        let (first, _) = discount_duplicates(nodes.clone(), Vec::new(), forwards);
        let (second, _) = discount_duplicates(nodes, Vec::new(), backwards);

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

        let (_, files) = discount_duplicates(nodes, vec![file("/vol/a", 100), file("/vol/b", 100)], links);

        assert_eq!(
            files.iter().map(|file| file.path.display().to_string()).collect::<Vec<_>>(),
            vec!["/vol/a".to_owned()]
        );
    }

    #[test]
    fn two_names_of_two_different_inodes_both_keep_their_bytes() {
        let nodes = vec![node("/vol", 300, 3)];
        let links = vec![sighting("/vol/a", 7, 100), sighting("/vol/b", 8, 100)];

        let (nodes, _) = discount_duplicates(nodes, Vec::new(), links);

        assert_eq!(sizes(&nodes), vec![("/vol".to_owned(), 300, 3)]);
    }

    #[test]
    fn three_names_of_one_inode_leave_one_copy_behind() {
        let nodes = vec![node("/vol", 300, 3)];
        let links = vec![sighting("/vol/c", 7, 100), sighting("/vol/a", 7, 100), sighting("/vol/b", 7, 100)];

        let (nodes, _) = discount_duplicates(nodes, Vec::new(), links);

        assert_eq!(sizes(&nodes), vec![("/vol".to_owned(), 100, 1)]);
    }

    #[test]
    fn a_subtraction_never_wraps_below_zero() {
        let nodes = vec![node("/vol", 10, 0)];
        let links = vec![sighting("/vol/a", 7, 100), sighting("/vol/b", 7, 100)];

        let (nodes, _) = discount_duplicates(nodes, Vec::new(), links);

        assert_eq!(sizes(&nodes), vec![("/vol".to_owned(), 0, 0)]);
    }
}
