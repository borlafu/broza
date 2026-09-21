//! The folder half of the human `scan` output (`docs/cli-spec.md` §3.1).
//!
//! Two shapes, one per volume: the default "largest consumers" list, and the
//! `--tree` view. Paths under the home directory are shortened to `~/…`, and a
//! directory is marked as such so a 300 GB `DerivedData` is not mistaken for
//! one file.

use std::fmt::Write as _;
use std::path::Path;

use broza::model::{ItemKind, LargestItem};
use broza::scan::TreeNode;

use crate::commands::scan::folders::VolumeTree;
use crate::output::format_bytes;

/// Indent of one tree level.
const TREE_INDENT: &str = "   ";
/// Suffix after a directory's path in the consumers list.
const DIRECTORY_MARK: &str = "/";
/// What an empty walk says instead of a heading with nothing under it.
const NOTHING_LISTED: &str = "  (nothing at or above --min-size)";

/// The consumers lists, one heading per walked volume.
pub fn render_largest(largest: &[LargestItem], trees: &[VolumeTree], home: Option<&Path>) -> String {
    trees
        .iter()
        .map(|volume| {
            let items: Vec<&LargestItem> =
                largest.iter().filter(|item| item.volume_id == volume.volume_id).collect();
            render_volume_largest(&volume.name, &items, home)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `Largest consumers on <volume>:` and its rows, biggest first.
fn render_volume_largest(name: &str, items: &[&LargestItem], home: Option<&Path>) -> String {
    let mut text = format!("Largest consumers on {name}:");
    if items.is_empty() {
        let _ignored = write!(text, "\n{NOTHING_LISTED}");
        return text;
    }
    let width = items.iter().map(|item| format_bytes(item.size_bytes).len()).max().unwrap_or(0);
    for item in items {
        let mark = if item.kind == ItemKind::Directory { DIRECTORY_MARK } else { "" };
        let _ignored = write!(
            text,
            "\n  {:>width$}  {}{mark}",
            format_bytes(item.size_bytes),
            abbreviate(&item.path, home)
        );
    }
    text
}

/// The `--tree` view, one nested block per walked volume.
pub fn render_trees(trees: &[VolumeTree], home: Option<&Path>) -> String {
    trees
        .iter()
        .map(|volume| {
            let mut text = format!("Folder tree of {}:", volume.name);
            render_node(&mut text, &volume.tree.root, 0, home);
            text
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// One node and, indented below it, its children largest first.
fn render_node(text: &mut String, node: &TreeNode, level: usize, home: Option<&Path>) {
    let indent = TREE_INDENT.repeat(level);
    let label = if level == 0 { abbreviate(&node.path, home) } else { node.name.clone() };
    let _ignored = write!(
        text,
        "\n{indent}{:>10}  {:>5.1}%  {label}/",
        format_bytes(node.size_bytes),
        node.percent_of_parent
    );
    for child in &node.children {
        render_node(text, child, level + 1, home);
    }
    if !node.children.is_empty() && node.other_bytes > 0 {
        let _ignored = write!(
            text,
            "\n{indent}{TREE_INDENT}{:>10}         (files and smaller folders)",
            format_bytes(node.other_bytes)
        );
    }
}

/// `~/…` for a path under the home directory, in either firmlink spelling.
pub fn abbreviate(path: &Path, home: Option<&Path>) -> String {
    let Some(home) = home else { return path.display().to_string() };
    let data_twin = Path::new("/System/Volumes/Data").join(home.strip_prefix("/").unwrap_or(home));
    for prefix in [home, data_twin.as_path()] {
        if let Ok(rest) = path.strip_prefix(prefix) {
            return if rest.as_os_str().is_empty() {
                "~".to_owned()
            } else {
                format!("~/{}", rest.display())
            };
        }
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::PathBuf;

    use broza::model::VolumeId;
    use broza::scan::TreeView;

    use super::*;

    fn id() -> VolumeId {
        "disk3s5".parse().unwrap()
    }

    fn item(path: &str, size: u64, kind: ItemKind) -> LargestItem {
        LargestItem { path: PathBuf::from(path), size_bytes: size, kind, volume_id: id() }
    }

    fn leaf(name: &str, path: &str, size: u64, percent: f64) -> TreeNode {
        TreeNode {
            name: name.to_owned(),
            path: PathBuf::from(path),
            size_bytes: size,
            percent_of_parent: percent,
            other_bytes: size,
            children: Vec::new(),
        }
    }

    fn volume(tree: TreeView) -> VolumeTree {
        VolumeTree { volume_id: id(), name: "Macintosh HD - Data".to_owned(), tree }
    }

    #[test]
    fn the_consumers_list_shortens_home_and_marks_directories() {
        let home = Path::new("/Users/dana");
        let largest = vec![
            item("/Users/dana/Library/Developer", 312_400_000_000, ItemKind::Directory),
            item("/System/Volumes/Data/Users/dana/Movies/film.mov", 61_700_000_000, ItemKind::File),
        ];
        let trees = vec![volume(TreeView { root: leaf("/", "/", 0, 100.0) })];

        let text = render_largest(&largest, &trees, Some(home));

        assert_eq!(
            text,
            "Largest consumers on Macintosh HD - Data:\n  312.4 GB  ~/Library/Developer/\n   61.7 GB  ~/Movies/film.mov"
        );
    }

    #[test]
    fn an_empty_list_says_so_instead_of_printing_a_bare_heading() {
        let trees = vec![volume(TreeView { root: leaf("/", "/", 0, 100.0) })];

        let text = render_largest(&[], &trees, None);

        assert!(text.ends_with(NOTHING_LISTED), "{text}");
    }

    #[test]
    fn the_tree_view_nests_children_and_accounts_for_the_rest() {
        let mut root = leaf("/System/Volumes/Data", "/System/Volumes/Data", 1000, 100.0);
        root.other_bytes = 200;
        root.children = vec![leaf("Users", "/System/Volumes/Data/Users", 800, 80.0)];
        let trees = vec![volume(TreeView { root })];

        let text = render_trees(&trees, Some(Path::new("/Users/dana")));

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Folder tree of Macintosh HD - Data:");
        assert!(lines[1].contains("100.0%  /System/Volumes/Data/"), "{}", lines[1]);
        assert!(lines[2].contains(" 80.0%  Users/"), "{}", lines[2]);
        assert!(lines[3].contains("(files and smaller folders)"), "{}", lines[3]);
    }

    #[test]
    fn abbreviation_handles_both_spellings_and_the_home_itself() {
        let home = Some(Path::new("/Users/dana"));

        assert_eq!(abbreviate(Path::new("/Users/dana"), home), "~");
        assert_eq!(abbreviate(Path::new("/Users/dana/x"), home), "~/x");
        assert_eq!(abbreviate(Path::new("/System/Volumes/Data/Users/dana/x"), home), "~/x");
        assert_eq!(abbreviate(Path::new("/Users/other/x"), home), "/Users/other/x");
        assert_eq!(abbreviate(Path::new("/Users/dana/x"), None), "/Users/dana/x");
    }
}
