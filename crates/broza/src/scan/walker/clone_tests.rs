//! What the [walker](super) must do with APFS clones, over the in-memory filesystem.
//!
//! A family of clones shares one set of blocks. The original — the file whose
//! clone id is its own inode — keeps the bytes when it is still there; when it
//! is gone, the clone whose path sorts first does, so two runs agree.

use std::path::Path;

use super::WalkOptions;
use super::tests::{node, sample, walk_sample};
use crate::ports::FileOps;
use crate::testing::FakeFileOps;

/// The sample tree with `f1` cloned into `a/` and into `b/`, original removed
/// when `keep_original` is `false`.
fn with_family(keep_original: bool) -> FakeFileOps {
    let fs = sample().with_clone("/vol/a/f1", "/vol/a/f1-copy").with_clone("/vol/a/f1", "/vol/b/f1-copy");
    if !keep_original {
        fs.remove_tree(Path::new("/vol/a/f1")).unwrap_or_else(|e| panic!("remove the original: {e}"));
    }
    fs
}

#[test]
fn clones_of_a_file_that_still_exists_are_counted_nowhere() {
    let fs = with_family(true);

    let result = walk_sample(&fs, &WalkOptions::default());

    assert_eq!(node(&result, "/vol/a").size_bytes, 6000, "the original keeps the bytes");
    assert_eq!(node(&result, "/vol/a").file_count, 3, "a discounted clone is not counted as a file either");
    assert_eq!(node(&result, "/vol/b").size_bytes, 5000);
    assert_eq!(node(&result, "/vol/b").file_count, 1);
    assert_eq!(node(&result, "/vol").size_bytes, 11_000);
}

#[test]
fn clones_whose_original_is_gone_are_counted_once_at_the_first_path() {
    let fs = with_family(false);

    let result = walk_sample(&fs, &WalkOptions::default());

    assert_eq!(node(&result, "/vol/a").size_bytes, 6000, "a/f1-copy sorts first and keeps the bytes");
    assert_eq!(node(&result, "/vol/b").size_bytes, 5000);
    assert_eq!(node(&result, "/vol").size_bytes, 11_000, "the family counts once");
    assert_eq!(node(&result, "/vol").file_count, 4);
}

#[test]
fn two_walks_over_one_family_credit_the_same_directory() {
    let fs = with_family(false);

    let first = walk_sample(&fs, &WalkOptions::default());
    let second = walk_sample(&fs, &WalkOptions::default());

    assert_eq!(first, second);
}

#[test]
fn clones_leave_every_directory_cacheable_and_the_kept_family_is_reported() {
    let fs = with_family(false);

    let result = walk_sample(&fs, &WalkOptions::default());

    assert!(result.nodes.iter().all(|node| !node.has_hard_links), "{:?}", result.nodes);
    assert_eq!(result.kept_clones.len(), 1);
    assert_eq!(result.kept_clones[0].0, Path::new("/vol/a"), "the keeper's directory");
}

#[test]
fn a_family_kept_in_a_served_subtree_discounts_the_clone_the_walk_meets() {
    use super::tests::subtree;
    use crate::scan::walker::{CachedSubtree, DirIdentity};

    let fs = with_family(false);
    let cold = walk_sample(&fs, &WalkOptions::default());
    let family = cold.kept_clones[0].1;
    // Serve `a`, the keeper's directory, as the cache would: its aggregate,
    // and the family it keeps.
    let hook = |identity: &DirIdentity| {
        (identity.path == Path::new("/vol/a"))
            .then(|| CachedSubtree { kept_clones: vec![family], ..subtree(identity, 6000) })
    };
    let options = WalkOptions { skip_hook: Some(&hook), ..WalkOptions::default() };

    let warm = walk_sample(&fs, &options);

    assert_eq!(node(&warm, "/vol/a").size_bytes, 6000, "served");
    assert_eq!(node(&warm, "/vol/b").size_bytes, 5000, "b's clone is discounted, as the cold walk did");
    assert_eq!(node(&warm, "/vol").size_bytes, 11_000, "the family still counts once");
}

#[test]
fn a_discounted_clone_is_reported_as_freeing_nothing_and_leaves_the_largest_item() {
    let fs = with_family(true);
    let options =
        WalkOptions { report_files_min_size: Some(1), report_files_top: 10, ..WalkOptions::default() };

    let result = walk_sample(&fs, &options);

    let paths: Vec<String> = result.files.iter().map(|file| file.path.display().to_string()).collect();
    assert!(paths.contains(&"/vol/a/f1".to_owned()), "{paths:?}");
    let copies: Vec<(&str, u64, bool)> = result
        .files
        .iter()
        .filter(|file| file.path.ends_with("f1-copy"))
        .map(|file| (file.path.to_str().unwrap_or(""), file.allocated_bytes, file.is_clone()))
        .collect();
    assert_eq!(
        copies,
        vec![("/vol/a/f1-copy", 0, true), ("/vol/b/f1-copy", 0, true)],
        "listed, freeing nothing"
    );
    let original = result
        .files
        .iter()
        .find(|file| file.path == Path::new("/vol/a/f1"))
        .unwrap_or_else(|| panic!("f1 is reported: {paths:?}"));
    assert_eq!(original.clone_id, Some(original.inode), "a file carries its clone id into the report");
    assert_eq!(node(&result, "/vol/b").largest_item_bytes, 8192, "big, not the discounted copy");
}

#[test]
fn a_hard_linked_clone_is_settled_as_a_hard_link_only() {
    // Two names of one clone: the names settle to one, and that one is counted
    // where it stands, beside the original. Rare enough to be documented
    // rather than solved (ADR 0009).
    let fs =
        sample().with_clone("/vol/a/f1", "/vol/b/copy").with_hard_link("/vol/b/copy", "/vol/b/copy-again");

    let result = walk_sample(&fs, &WalkOptions::default());

    assert_eq!(node(&result, "/vol/b").size_bytes, 6000);
    assert_eq!(node(&result, "/vol").size_bytes, 12_000);
}
