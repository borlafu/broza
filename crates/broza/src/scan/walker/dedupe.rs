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
///
/// The sightings with one name — clones, which [`super::clones`] settles next —
/// pass straight through as credited: a million of them is the ordinary case
/// on a disk with a cloned media folder, and grouping them by inode would
/// find a million groups of one.
pub(super) fn settle_hard_links(
    nodes: Vec<DirNode>,
    files: Vec<FileEntry>,
    links: Vec<LinkSighting>,
) -> Settled {
    let (linked, single): (Vec<LinkSighting>, Vec<LinkSighting>) =
        links.into_iter().partition(|link| link.link_count > 1);
    if linked.is_empty() {
        return Settled { nodes, files, credited: single, dropped: Vec::new() };
    }
    let nodes = mark_uncacheable(nodes, &hiders(&linked));
    let (duplicates, mut credited) = partition_names(linked);
    credited.extend(single);
    if duplicates.is_empty() {
        return Settled { nodes, files, credited, dropped: Vec::new() };
    }
    let dropped: HashSet<&Path> = duplicates.iter().map(|link| link.path.as_path()).collect();
    let kept_files = files.into_iter().filter(|file| !dropped.contains(file.path.as_path())).collect();
    let dropped = duplicates.iter().map(|link| link.path.clone()).collect();
    Settled { nodes: subtract(nodes, &duplicates), files: kept_files, credited, dropped }
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
    /// The names that were discounted: other lists of files drop them too.
    pub dropped: Vec<PathBuf>,
}

/// Directories that hold some, but not all, of an inode's names.
///
/// For an inode whose names were all seen, those are the directories between
/// each name and the lowest directory that contains every name: anything above
/// that holds the whole set and is free to be cached. For an inode with names
/// the walk never saw — another volume, an excluded path, a subtree that was
/// itself served from the cache — no directory can be sure it holds them all.
pub(super) fn hiders(links: &[LinkSighting]) -> HashSet<PathBuf> {
    hiders_of(
        links,
        |link| (link.device, link.inode),
        |group| {
            u64::try_from(group.len())
                .is_ok_and(|seen| Some(seen) == group.first().map(|link| link.link_count))
        },
    )
}

/// [`hiders`] over any grouping: `key` says which sightings share their bytes,
/// `complete` whether a group is known to be the whole set.
///
/// Borrows the sightings rather than re-keying a copy of them: a folder of a
/// million clones is exactly where a second copy would hurt.
pub(super) fn hiders_of(
    links: &[LinkSighting],
    key: impl Fn(&LinkSighting) -> (u64, u64),
    complete: impl Fn(&[&LinkSighting]) -> bool,
) -> HashSet<PathBuf> {
    let mut hiders: HashSet<PathBuf> = HashSet::new();
    for (_, group) in group_by(links, key) {
        let whole_set = complete(&group).then(|| lowest_common_directory(&group)).flatten();
        for link in &group {
            for ancestor in link.path.ancestors().skip(1) {
                if whole_set.as_deref() == Some(ancestor) {
                    break;
                }
                if !hiders.contains(ancestor) {
                    hiders.insert(ancestor.to_path_buf());
                }
            }
        }
    }
    hiders
}

/// The sightings sharing each key, grouped.
fn group_by(
    links: &[LinkSighting],
    key: impl Fn(&LinkSighting) -> (u64, u64),
) -> HashMap<(u64, u64), Vec<&LinkSighting>> {
    links.iter().fold(HashMap::new(), |mut groups, link| {
        groups.entry(key(link)).or_insert_with(Vec::new).push(link);
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
pub(super) fn mark_uncacheable(nodes: Vec<DirNode>, hiders: &HashSet<PathBuf>) -> Vec<DirNode> {
    nodes
        .into_iter()
        .map(|node| if hiders.contains(&node.path) { node.hiding_a_name() } else { node })
        .collect()
}

/// Every sighting that is not the first name of its inode, by path.
fn partition_names(links: Vec<LinkSighting>) -> (Vec<LinkSighting>, Vec<LinkSighting>) {
    let mut links = links;
    links.sort_by(|left, right| {
        (left.device, left.inode, &left.path).cmp(&(right.device, right.inode, &right.path))
    });
    let mut keeper: Option<(u64, u64)> = None;
    links.into_iter().partition(|link| {
        let inode = (link.device, link.inode);
        if keeper == Some(inode) {
            return true;
        }
        keeper = Some(inode);
        false
    })
}

/// Remove each duplicate's bytes from its directory and every directory above.
///
/// Linear in the duplicates and in the nodes: each duplicate is charged to
/// its own directory, and the charges then flow up the tree once, deepest
/// directories first. Walking every duplicate's ancestors one by one would
/// cost a hash lookup per ancestor per duplicate, and a folder of a million
/// clones has a million duplicates a dozen levels deep.
pub(super) fn subtract(nodes: Vec<DirNode>, duplicates: &[LinkSighting]) -> Vec<DirNode> {
    if duplicates.is_empty() {
        return nodes;
    }
    let mut charged: HashMap<&Path, Charge> = HashMap::new();
    for duplicate in duplicates {
        let Some(dir) = duplicate.path.parent() else { continue };
        let charge = charged.entry(dir).or_default();
        *charge = charge.plus(&Charge::of(duplicate));
    }
    apply_charges(nodes, &charged)
}

/// Take each directory's charge out of it and out of every directory above.
pub(super) fn apply_charges(nodes: Vec<DirNode>, per_dir: &HashMap<&Path, Charge>) -> Vec<DirNode> {
    if per_dir.is_empty() {
        return nodes;
    }
    let index: HashMap<&Path, usize> =
        nodes.iter().enumerate().map(|(at, node)| (node.path.as_path(), at)).collect();
    let mut charges = vec![Charge::default(); nodes.len()];
    for (dir, charge) in per_dir {
        if let Some(at) = index.get(dir) {
            charges[*at] = charges[*at].plus(charge);
        }
    }
    let mut deepest_first: Vec<usize> = (0..nodes.len()).collect();
    deepest_first.sort_by_key(|at| std::cmp::Reverse(nodes[*at].path.components().count()));
    for at in deepest_first {
        let Some(parent) = nodes[at].path.parent().and_then(|parent| index.get(parent)) else { continue };
        charges[*parent] = charges[*parent].plus(&charges[at]);
    }
    drop(index);
    nodes
        .into_iter()
        .zip(charges)
        .map(|(node, charge)| DirNode {
            size_bytes: node.size_bytes.saturating_sub(charge.size_bytes),
            allocated_bytes: node.allocated_bytes.saturating_sub(charge.allocated_bytes),
            file_count: node.file_count.saturating_sub(charge.files),
            ..node
        })
        .collect()
}

/// What a directory's subtree is charged for its discounted names.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Charge {
    /// Apparent bytes to take out.
    pub size_bytes: u64,
    /// Allocated bytes to take out.
    pub allocated_bytes: u64,
    /// Files to take out of the count.
    pub files: u64,
}

impl Charge {
    fn of(duplicate: &LinkSighting) -> Self {
        Self { size_bytes: duplicate.size_bytes, allocated_bytes: duplicate.allocated_bytes, files: 1 }
    }

    /// Both charges together, saturating.
    pub fn plus(self, other: &Self) -> Self {
        Self {
            size_bytes: self.size_bytes.saturating_add(other.size_bytes),
            allocated_bytes: self.allocated_bytes.saturating_add(other.allocated_bytes),
            files: self.files.saturating_add(other.files),
        }
    }

    /// This charge less `other`, saturating.
    pub fn minus(self, other: &Self) -> Self {
        Self {
            size_bytes: self.size_bytes.saturating_sub(other.size_bytes),
            allocated_bytes: self.allocated_bytes.saturating_sub(other.allocated_bytes),
            files: self.files.saturating_sub(other.files),
        }
    }
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
            clone_id: None,
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
        FileEntry::sized(path, size_bytes)
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
