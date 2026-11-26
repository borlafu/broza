//! The folder half of the human `scan` output (`docs/cli-spec.md` §3.1).
//!
//! Two shapes, one per volume: the default "largest consumers" list, and the
//! `--tree` view. Paths under the home directory are shortened to `~/…`, and a
//! directory is marked as such so a 300 GB `DerivedData` is not mistaken for
//! one file. `scan -i` numbers the directories, across volumes in print order,
//! so the user can open one by its number.

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
/// Width of the number column of a numbered list.
const NUMBER_WIDTH: usize = 4;
/// Characters a path may hold and still be pasted into a shell unquoted.
const SHELL_SAFE: &str = "/._-~+,:@%";

/// The consumers lists, one heading per walked volume.
pub fn render_largest(largest: &[LargestItem], trees: &[VolumeTree], home: Option<&Path>) -> String {
    render_lists(largest, trees, home, false)
}

/// [`render_largest`] with every directory numbered, for `scan -i`; the
/// numbers index [`browsable`].
pub fn render_largest_numbered(largest: &[LargestItem], trees: &[VolumeTree], home: Option<&Path>) -> String {
    render_lists(largest, trees, home, true)
}

/// The directories of the consumers lists, in the order they are printed.
pub fn browsable<'a>(largest: &'a [LargestItem], trees: &[VolumeTree]) -> Vec<&'a LargestItem> {
    trees
        .iter()
        .flat_map(|volume| {
            largest
                .iter()
                .filter(move |item| item.volume_id == volume.volume_id && item.kind == ItemKind::Directory)
        })
        .collect()
}

/// The line under the consumers list that says how to look inside one.
pub fn drill_down_hint(largest: &[LargestItem], trees: &[VolumeTree], home: Option<&Path>) -> Option<String> {
    let first = browsable(largest, trees).into_iter().next()?;
    Some(format!(
        "Look inside one: broza scan {}   ·   browse them all: broza scan -i",
        shell_word(&first.path, home)
    ))
}

/// `path` as one shell word: `~/…` when it needs no quoting, else quoted in full.
fn shell_word(path: &Path, home: Option<&Path>) -> String {
    let short = abbreviate(path, home);
    if short.chars().all(|c| c.is_ascii_alphanumeric() || SHELL_SAFE.contains(c)) {
        return short;
    }
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn render_lists(
    largest: &[LargestItem],
    trees: &[VolumeTree],
    home: Option<&Path>,
    numbered: bool,
) -> String {
    let mut next = 1;
    let mut lists = Vec::new();
    for volume in trees {
        let items: Vec<&LargestItem> =
            largest.iter().filter(|item| item.volume_id == volume.volume_id).collect();
        let heading = format!("Largest consumers on {}:", volume.name);
        lists.push(render_listing(&heading, &items, home, numbered.then_some(&mut next)));
    }
    lists.join("\n\n")
}

/// `heading` and its rows, biggest first; with `numbers`, each directory gets
/// the next number.
pub fn render_listing(
    heading: &str,
    items: &[&LargestItem],
    home: Option<&Path>,
    mut numbers: Option<&mut usize>,
) -> String {
    let mut text = heading.to_owned();
    if items.is_empty() {
        let _ignored = write!(text, "\n{NOTHING_LISTED}");
        return text;
    }
    let width = items.iter().map(|item| format_bytes(item.size_bytes).len()).max().unwrap_or(0);
    for item in items {
        let is_dir = item.kind == ItemKind::Directory;
        let number = match numbers.as_deref_mut() {
            Some(next) if is_dir => {
                let label = format!("{next:>NUMBER_WIDTH$}");
                *next += 1;
                label
            }
            Some(_) => " ".repeat(NUMBER_WIDTH),
            None => String::new(),
        };
        let mark = if is_dir { DIRECTORY_MARK } else { "" };
        let _ignored = write!(
            text,
            "\n{number}  {:>width$}  {}{mark}",
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
    fn numbered_lists_count_directories_across_volumes_and_skip_files() {
        let largest = vec![
            item("/Users/dana/Library", 300, ItemKind::Directory),
            item("/Users/dana/film.mov", 200, ItemKind::File),
            item("/Users/dana/code", 100, ItemKind::Directory),
        ];
        let trees = vec![volume(TreeView { root: leaf("/", "/", 0, 100.0) })];

        let text = render_largest_numbered(&largest, &trees, Some(Path::new("/Users/dana")));

        assert_eq!(
            text,
            "Largest consumers on Macintosh HD - Data:\n   1  300 B  ~/Library/\n      200 B  ~/film.mov\n   2  100 B  ~/code/"
        );
        let dirs: Vec<&Path> = browsable(&largest, &trees).iter().map(|i| i.path.as_path()).collect();
        assert_eq!(dirs, vec![Path::new("/Users/dana/Library"), Path::new("/Users/dana/code")]);
    }

    #[test]
    fn every_number_printed_is_the_folder_browsable_puts_at_that_index() {
        let other: VolumeId = "disk5s1".parse().unwrap();
        let empty: VolumeId = "disk6s1".parse().unwrap();
        let on = |path: &str, kind, volume: &VolumeId| LargestItem {
            path: PathBuf::from(path),
            size_bytes: 1,
            kind,
            volume_id: volume.clone(),
        };
        let largest = vec![
            on("/a", ItemKind::Directory, &id()),
            on("/b", ItemKind::File, &id()),
            on("/c", ItemKind::Directory, &other),
            on("/d", ItemKind::Directory, &id()),
            on("/orphan", ItemKind::Directory, &"disk9s9".parse().unwrap()),
        ];
        let tree = |volume: &VolumeId, name: &str| VolumeTree {
            volume_id: volume.clone(),
            name: name.to_owned(),
            tree: TreeView { root: leaf("/", "/", 0, 100.0) },
        };
        let trees = vec![tree(&id(), "Data"), tree(&empty, "Empty"), tree(&other, "Other")];

        let text = render_largest_numbered(&largest, &trees, None);
        let printed: Vec<(usize, &str)> = text
            .lines()
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                let number = words.next()?.parse().ok()?;
                Some((number, words.nth(2)?))
            })
            .collect();
        let dirs: Vec<String> =
            browsable(&largest, &trees).iter().map(|item| format!("{}/", item.path.display())).collect();

        assert_eq!(printed.len(), dirs.len(), "{text}");
        for (number, path) in printed {
            assert_eq!(dirs[number - 1], path, "number {number} in:\n{text}");
        }
        assert!(text.contains("Largest consumers on Empty:\n  (nothing at or above --min-size)"), "{text}");
        assert!(!text.contains("/orphan"), "an item of no listed volume is neither printed nor numbered");
    }

    #[test]
    fn the_hint_quotes_a_path_only_when_a_shell_needs_it() {
        let trees = vec![volume(TreeView { root: leaf("/", "/", 0, 100.0) })];
        let plain = vec![item("/Users/dana/Library/Caches", 1, ItemKind::Directory)];
        let spaced = vec![item("/Users/dana/Library/Application Support", 1, ItemKind::Directory)];
        let home = Some(Path::new("/Users/dana"));

        assert_eq!(
            drill_down_hint(&plain, &trees, home).unwrap(),
            "Look inside one: broza scan ~/Library/Caches   ·   browse them all: broza scan -i"
        );
        assert!(
            drill_down_hint(&spaced, &trees, home)
                .unwrap()
                .contains("broza scan '/Users/dana/Library/Application Support'"),
        );
        assert!(
            drill_down_hint(&[item("/f", 1, ItemKind::File)], &trees, home).is_none(),
            "files cannot be opened"
        );
        for (raw, quoted) in [
            ("/x/it's", "'/x/it'\\''s'"),
            ("/x/$HOME dir", "'/x/$HOME dir'"),
            ("/x/a`b", "'/x/a`b'"),
            ("/x/a\nb", "'/x/a\nb'"),
        ] {
            let hint = drill_down_hint(&[item(raw, 1, ItemKind::Directory)], &trees, home).unwrap();
            assert!(hint.contains(&format!("broza scan {quoted}   ")), "{raw}: {hint}");
        }
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
