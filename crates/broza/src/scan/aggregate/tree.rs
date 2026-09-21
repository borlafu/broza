//! The nested view the human renderer draws (`docs/cli-spec.md` §3.1, `--tree`).
//!
//! Pure: it reshapes the flat list of [`DirNode`]s a walk produced into the nesting
//! the terminal shows, and computes nothing the walker did not measure.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::scan::walker::DirNode;

/// Percentage a root represents of itself.
const WHOLE_PERCENT: f64 = 100.0;

/// The tree of one scanned root.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeView {
    /// The scanned root.
    pub root: TreeNode,
}

/// One directory of the [`TreeView`].
#[derive(Debug, Clone, PartialEq)]
pub struct TreeNode {
    /// File name of the directory; the whole path for the root.
    pub name: String,
    /// Absolute path of the directory.
    pub path: PathBuf,
    /// Apparent bytes of the subtree.
    pub size_bytes: u64,
    /// Share of the parent, in percent; `100.0` for the root, `0.0` when the
    /// parent holds no bytes at all.
    pub percent_of_parent: f64,
    /// Bytes of this directory that none of the listed children account for:
    /// its own files, and the children left out by the depth or size limits.
    /// `children` plus `other_bytes` always add up to `size_bytes`.
    pub other_bytes: u64,
    /// Children above the minimum size, largest first.
    pub children: Vec<TreeNode>,
}

/// Nest `nodes` under `root`, down to `depth` levels below it.
///
/// `depth` counts levels **below** the root, matching `--depth`: `0` is the root
/// alone. Children smaller than `min_size` are left out of the listing and end
/// up in their parent's `other_bytes`, so the arithmetic on screen adds up.
pub fn tree(root: &DirNode, nodes: &[DirNode], depth: usize, min_size: u64) -> TreeView {
    let index = index_by_parent(nodes);
    let built = build(root, WHOLE_PERCENT, depth, min_size, &index);
    // The root is the only node shown with its whole path: it is where the scan
    // started, and `Data` alone would not tell the user which volume that is.
    TreeView { root: TreeNode { name: root.path.display().to_string(), ..built } }
}

/// Children of every directory, keyed by the parent's path.
fn index_by_parent(nodes: &[DirNode]) -> BTreeMap<&Path, Vec<&DirNode>> {
    nodes.iter().fold(BTreeMap::new(), |mut index, node| {
        if let Some(parent) = node.path.parent() {
            index.entry(parent).or_default().push(node);
        }
        index
    })
}

/// Build one node and, while `depth` allows, everything below it.
fn build(
    node: &DirNode,
    percent_of_parent: f64,
    depth: usize,
    min_size: u64,
    index: &BTreeMap<&Path, Vec<&DirNode>>,
) -> TreeNode {
    let children: Vec<TreeNode> = if depth == 0 {
        Vec::new()
    } else {
        children_of(node, min_size, index)
            .into_iter()
            .map(|child| build(child, share(child.size_bytes, node.size_bytes), depth - 1, min_size, index))
            .collect()
    };
    let listed: u64 = children.iter().map(|child| child.size_bytes).sum();
    TreeNode {
        name: name_of(&node.path),
        path: node.path.clone(),
        size_bytes: node.size_bytes,
        percent_of_parent,
        other_bytes: node.size_bytes.saturating_sub(listed),
        children,
    }
}

/// Children of `node` above `min_size`, largest first and then by name.
fn children_of<'a>(
    node: &DirNode,
    min_size: u64,
    index: &BTreeMap<&'a Path, Vec<&'a DirNode>>,
) -> Vec<&'a DirNode> {
    let mut children: Vec<&DirNode> = index
        .get(node.path.as_path())
        .map(|children| children.iter().copied().filter(|child| child.size_bytes >= min_size).collect())
        .unwrap_or_default();
    children.sort_by(|left, right| right.size_bytes.cmp(&left.size_bytes).then(left.path.cmp(&right.path)));
    children
}

/// Share of `parent` that `size` represents, in percent.
#[expect(
    clippy::cast_precision_loss,
    reason = "a percentage is presentation, and f64 holds byte counts far beyond any disk exactly enough for it"
)]
fn share(size: u64, parent: u64) -> f64 {
    if parent == 0 {
        return 0.0;
    }
    size as f64 * WHOLE_PERCENT / parent as f64
}

/// Name shown for `path`: its file name, or the whole path when it has none.
fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{TreeNode, tree};
    use crate::scan::walker::DirNode;

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
            inode: 10,
            mtime: None,
            children_truncated: false,
        }
    }

    fn sample() -> Vec<DirNode> {
        vec![
            dir("/vol", 1000),
            dir("/vol/a", 600),
            dir("/vol/a/deep", 500),
            dir("/vol/b", 300),
            dir("/vol/c", 10),
        ]
    }

    fn names(node: &TreeNode) -> Vec<&str> {
        node.children.iter().map(|child| child.name.as_str()).collect()
    }

    fn root_of(nodes: &[DirNode], depth: usize, min_size: u64) -> TreeNode {
        let root = nodes.first().unwrap_or_else(|| panic!("no nodes"));
        tree(root, nodes, depth, min_size).root
    }

    #[test]
    fn children_are_nested_under_their_parent_largest_first() {
        let root = root_of(&sample(), 2, 0);

        assert_eq!(root.name, "/vol");
        assert_eq!(root.size_bytes, 1000);
        assert_eq!(names(&root), vec!["a", "b", "c"]);
        assert_eq!(names(&root.children[0]), vec!["deep"]);
    }

    #[test]
    fn every_child_carries_its_share_of_the_parent() {
        let root = root_of(&sample(), 2, 0);

        assert!((root.percent_of_parent - 100.0).abs() < f64::EPSILON);
        assert!((root.children[0].percent_of_parent - 60.0).abs() < f64::EPSILON);
        assert!((root.children[0].children[0].percent_of_parent - 500.0 / 6.0).abs() < 0.001);
    }

    #[test]
    fn what_the_children_do_not_account_for_is_reported_as_the_rest() {
        let root = root_of(&sample(), 2, 0);

        assert_eq!(root.other_bytes, 1000 - 600 - 300 - 10);
        assert_eq!(root.children[0].other_bytes, 600 - 500);
        let accounted: u64 = root.children.iter().map(|child| child.size_bytes).sum();
        assert_eq!(accounted + root.other_bytes, root.size_bytes);
    }

    #[test]
    fn children_left_out_by_a_limit_end_up_in_the_rest() {
        let shallow = root_of(&sample(), 0, 0);
        let filtered = root_of(&sample(), 2, 100);

        assert_eq!(shallow.other_bytes, 1000, "with no children listed, everything is the rest");
        assert_eq!(filtered.other_bytes, 1000 - 600 - 300, "the 10-byte child is in the rest");
    }

    #[test]
    fn the_depth_limit_counts_levels_below_the_root() {
        let root = root_of(&sample(), 1, 0);

        assert_eq!(names(&root), vec!["a", "b", "c"]);
        assert!(root.children[0].children.is_empty());
        assert_eq!(root_of(&sample(), 0, 0).children.len(), 0);
    }

    #[test]
    fn children_below_the_minimum_size_are_left_out() {
        let root = root_of(&sample(), 2, 100);

        assert_eq!(names(&root), vec!["a", "b"]);
    }

    #[test]
    fn a_small_root_is_still_the_root() {
        let root = root_of(&[dir("/vol", 5)], 2, 100);

        assert_eq!(root.size_bytes, 5);
        assert!(root.children.is_empty());
    }

    #[test]
    fn a_volume_mounted_at_the_root_is_named_by_its_path() {
        let nodes = vec![dir("/", 10), dir("/Users", 4)];

        let root = root_of(&nodes, 1, 0);

        assert_eq!(root.name, "/");
        assert_eq!(names(&root), vec!["Users"]);
    }

    #[test]
    fn a_walk_with_only_a_root_has_a_tree_of_one_node() {
        let only = dir("/vol", 7);

        let view = tree(&only, std::slice::from_ref(&only), 3, 0);

        assert_eq!(view.root.size_bytes, 7);
        assert!(view.root.children.is_empty());
    }

    #[test]
    fn a_parent_of_zero_bytes_gives_its_children_no_share_instead_of_dividing_by_zero() {
        let nodes = vec![dir("/vol", 0), dir("/vol/a", 0)];

        let root = root_of(&nodes, 1, 0);

        assert!((root.children[0].percent_of_parent - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn nodes_that_are_not_below_the_root_are_ignored() {
        let nodes = vec![dir("/vol", 10), dir("/vol/a", 4), dir("/elsewhere/x", 9)];

        let root = root_of(&nodes, 3, 0);

        assert_eq!(names(&root), vec!["a"]);
    }
}
