//! What the [walker](super) must do, over the in-memory filesystem.
//!
//! Beside the code it covers, in its own file only because `walker.rs` and this
//! grew past what one file should hold.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use super::{DirIdentity, DirNode, WalkOptions, WalkResult, walk};
use crate::scan::cache::{CacheKey, DirRecord};
use crate::scan::progress::ProgressReporter;
use crate::testing::{FakeFileOps, FixedClock};

/// How many files a test keeps, when it asks for files at all.
const KEEP_FILES: usize = 10;

/// A cache record standing for a subtree of `size_bytes`.
pub(super) fn record(identity: &DirIdentity, size_bytes: u64) -> DirRecord {
    DirRecord {
        key: CacheKey::of(identity).unwrap_or_else(|| panic!("no key for {identity:?}")),
        size_bytes,
        allocated_bytes: size_bytes * 2,
        file_count: 3,
        dir_count: 1,
        dataless_count: 0,
        largest_item_bytes: size_bytes,
        has_hard_links: false,
        has_truncation: false,
        recorded_at: Timestamp::UNIX_EPOCH,
    }
}

/// Apparent size of every file of the sample tree, by path.
const SAMPLE: [(&str, u64); 4] =
    [("/vol/a/f1", 1000), ("/vol/a/f2", 2000), ("/vol/a/sub/f3", 3000), ("/vol/b/big", 5000)];

pub(super) fn sample() -> FakeFileOps {
    SAMPLE
        .iter()
        .fold(FakeFileOps::new().with_root("/vol", 1), |fs, (path, size)| fs.with_sized_file(path, *size))
}

pub(super) fn node<'a>(result: &'a WalkResult, path: &str) -> &'a DirNode {
    result
        .nodes
        .iter()
        .find(|node| node.path == Path::new(path))
        .unwrap_or_else(|| panic!("no node for {path} in {:?}", result.paths()))
}

pub(super) fn walk_sample(fs: &FakeFileOps, options: &WalkOptions<'_>) -> WalkResult {
    walk(Path::new("/vol"), options, fs)
}

#[test]
fn every_directory_becomes_a_node_carrying_its_whole_subtree() {
    let result = walk_sample(&sample(), &WalkOptions::default());

    assert_eq!(
        result.paths(),
        vec![
            PathBuf::from("/vol"),
            PathBuf::from("/vol/a"),
            PathBuf::from("/vol/a/sub"),
            PathBuf::from("/vol/b"),
        ]
    );
    let root = node(&result, "/vol");
    assert_eq!(root.size_bytes, 11_000);
    assert_eq!(root.file_count, 4);
    assert_eq!(root.dir_count, 3);
    assert_eq!(node(&result, "/vol/a").size_bytes, 6000);
    assert_eq!(node(&result, "/vol/a").dir_count, 1);
    assert_eq!(node(&result, "/vol/a/sub").file_count, 1);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(!root.children_truncated);
}

#[test]
fn a_node_carries_the_identity_the_cache_is_keyed_by() {
    let fs = sample();
    let result = walk_sample(&fs, &WalkOptions::default());

    let root = node(&result, "/vol");
    assert_eq!(root.device, 1);
    assert!(root.inode > 0);
    assert!(root.mtime.is_some());
    assert_eq!(result.root().map(|node| node.path.clone()), Some(PathBuf::from("/vol")));
}

#[test]
fn allocated_bytes_count_whole_blocks_and_never_shrink_below_the_apparent_size() {
    let result = walk_sample(&sample(), &WalkOptions::default());

    let leaf = node(&result, "/vol/b");
    assert_eq!(leaf.size_bytes, 5000);
    assert_eq!(leaf.allocated_bytes, 8192);
}

#[test]
fn two_names_of_one_file_in_one_directory_are_counted_once() {
    let fs = sample().with_hard_link("/vol/a/f1", "/vol/a/f1-again");

    let result = walk_sample(&fs, &WalkOptions::default());

    assert_eq!(node(&result, "/vol/a").size_bytes, 6000);
    assert_eq!(node(&result, "/vol/a").file_count, 3);
    assert_eq!(node(&result, "/vol").size_bytes, 11_000);
}

#[test]
fn a_hard_link_in_another_directory_is_still_counted_once_in_the_total() {
    let fs = sample().with_hard_link("/vol/a/f1", "/vol/b/f1-again");

    let result = walk_sample(&fs, &WalkOptions::default());

    assert_eq!(node(&result, "/vol").size_bytes, 11_000);
    assert_eq!(node(&result, "/vol/a").size_bytes + node(&result, "/vol/b").size_bytes, 11_000);
}

#[test]
fn a_symlink_counts_as_its_own_size_and_is_never_followed() {
    let fs = sample().with_symlink("/vol/a/link", "/vol/b");

    let result = walk_sample(&fs, &WalkOptions::default());

    assert_eq!(node(&result, "/vol/a").size_bytes, 6000 + "/vol/b".len() as u64);
    assert_eq!(node(&result, "/vol/a").file_count, 4);
    assert_eq!(node(&result, "/vol").dir_count, 3);
}

#[test]
fn an_unreadable_subtree_becomes_a_warning_and_the_walk_continues() {
    let fs = sample().with_denied("/vol/a");

    let result = walk_sample(&fs, &WalkOptions::default());

    assert_eq!(node(&result, "/vol").size_bytes, 5000);
    assert!(node(&result, "/vol").children_truncated);
    let denied = result.errors.first().unwrap_or_else(|| panic!("no warning: {result:?}"));
    assert_eq!(denied.code, super::PERMISSION_DENIED_CODE);
    assert_eq!(denied.path.as_deref(), Some(Path::new("/vol/a")));
}

#[test]
fn an_excluded_prefix_is_never_visited() {
    let options = WalkOptions { exclude: vec![PathBuf::from("/vol/a")], ..WalkOptions::default() };

    let result = walk_sample(&sample(), &options);

    assert_eq!(result.paths(), vec![PathBuf::from("/vol"), PathBuf::from("/vol/b")]);
    assert_eq!(node(&result, "/vol").size_bytes, 5000);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

#[test]
fn another_device_is_never_crossed_by_default() {
    let fs = sample();
    fs.add_root("/vol/b", 2);

    let result = walk_sample(&fs, &WalkOptions::default());

    assert_eq!(
        result.paths(),
        vec![PathBuf::from("/vol"), PathBuf::from("/vol/a"), PathBuf::from("/vol/a/sub")]
    );
    assert_eq!(node(&result, "/vol").size_bytes, 6000);
    assert!(node(&result, "/vol").children_truncated);
}

#[test]
fn crossing_devices_can_be_asked_for() {
    let fs = sample();
    fs.add_root("/vol/b", 2);
    let options = WalkOptions { same_device_only: false, ..WalkOptions::default() };

    let result = walk_sample(&fs, &options);

    assert_eq!(node(&result, "/vol").size_bytes, 11_000);
}

#[test]
fn a_depth_limit_hides_deep_nodes_without_losing_their_bytes() {
    let options = WalkOptions { max_depth: Some(1), ..WalkOptions::default() };

    let result = walk_sample(&sample(), &options);

    assert_eq!(result.paths(), vec![PathBuf::from("/vol"), PathBuf::from("/vol/a"), PathBuf::from("/vol/b")]);
    assert_eq!(node(&result, "/vol").size_bytes, 11_000);
    assert_eq!(node(&result, "/vol/a").size_bytes, 6000);
    assert!(node(&result, "/vol/a").children_truncated);
    assert!(!node(&result, "/vol").children_truncated);
}

#[test]
fn files_are_collected_only_above_the_reporting_threshold() {
    // The threshold is in allocated bytes, like every size the report prints;
    // the fake allocates in 4 KiB units, so only the 5000-byte file (8192
    // allocated) clears a 5000-byte threshold.
    let options = WalkOptions {
        report_files_min_size: Some(5000),
        report_files_top: KEEP_FILES,
        ..WalkOptions::default()
    };

    let result = walk_sample(&sample(), &options);

    let collected: Vec<_> = result.files.iter().map(|file| (file.path.clone(), file.size_bytes)).collect();
    assert_eq!(collected, vec![(PathBuf::from("/vol/b/big"), 5000)]);
}

#[test]
fn without_a_threshold_no_file_is_kept() {
    let result = walk_sample(&sample(), &WalkOptions::default());

    assert!(result.files.is_empty(), "{:?}", result.files);
}

#[test]
fn walking_a_missing_root_reports_one_error_and_nothing_else() {
    let result = walk(Path::new("/vol/ghost"), &WalkOptions::default(), &sample());

    assert!(result.nodes.is_empty());
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].path.as_deref(), Some(Path::new("/vol/ghost")));
    assert!(result.root().is_none());
}

#[test]
fn walking_a_file_reports_the_file_and_no_directory() {
    let options = WalkOptions {
        report_files_min_size: Some(0),
        report_files_top: KEEP_FILES,
        ..WalkOptions::default()
    };

    let result = walk(Path::new("/vol/b/big"), &options, &sample());

    assert!(result.nodes.is_empty());
    assert_eq!(result.files.len(), 1);
    assert_eq!(result.files[0].size_bytes, 5000);
}

#[test]
fn the_debug_of_the_options_shows_the_switches_and_hides_the_callbacks() {
    let options =
        WalkOptions { max_depth: Some(3), exclude: vec![PathBuf::from("/vol/a")], ..WalkOptions::default() };

    let shown = format!("{options:?}");

    assert!(shown.contains("max_depth: Some(3)"), "{shown}");
    assert!(shown.contains("same_device_only: true"), "{shown}");
    assert!(shown.contains("/vol/a"), "{shown}");
    assert!(!shown.contains("skip_hook"), "a closure has nothing to show: {shown}");
}

#[test]
fn a_cloud_placeholder_counts_as_an_entry_and_as_no_bytes() {
    let fs = sample().with_dataless_file("/vol/a/in-the-cloud.mov", 9_000_000);

    let result = walk_sample(&fs, &WalkOptions::default());

    let holder = node(&result, "/vol/a");
    assert_eq!(holder.size_bytes, 6000, "none of those bytes are on this disk");
    assert_eq!(holder.file_count, 4);
    assert_eq!(holder.dataless_count, 1);
    assert_eq!(node(&result, "/vol").dataless_count, 1, "the count adds up the tree");
}

#[test]
fn a_cloud_placeholder_directory_is_counted_and_never_opened() {
    let fs = sample();
    fs.add_file("/vol/cloud/inside.bin", &[]);
    fs.set_size("/vol/cloud/inside.bin", 40_000);
    fs.add_dataless_dir("/vol/cloud");

    let result = walk_sample(&fs, &WalkOptions::default());

    assert!(!result.paths().contains(&PathBuf::from("/vol/cloud")), "{:?}", result.paths());
    assert_eq!(node(&result, "/vol").size_bytes, 11_000, "what is inside it was not counted");
    assert_eq!(node(&result, "/vol").dataless_count, 1);
    assert!(node(&result, "/vol").children_truncated);
}

#[test]
fn a_placeholder_is_never_offered_as_a_largest_file() {
    let fs = sample().with_dataless_file("/vol/huge-in-the-cloud.mov", 9_000_000);
    let options = WalkOptions {
        report_files_min_size: Some(0),
        report_files_top: KEEP_FILES,
        ..WalkOptions::default()
    };

    let result = walk_sample(&fs, &options);

    assert!(
        result.files.iter().all(|file| !file.path.ends_with("huge-in-the-cloud.mov")),
        "{:?}",
        result.files
    );
}

#[test]
fn two_names_of_one_file_always_credit_the_same_directory() {
    let fs = sample().with_hard_link("/vol/a/f1", "/vol/b/f1-again");

    let first = walk_sample(&fs, &WalkOptions::default());
    let second = walk_sample(&fs, &WalkOptions::default());

    assert_eq!(first, second, "two runs of the same walk must agree");
    assert_eq!(node(&first, "/vol/a").size_bytes, 6000, "the first path keeps the bytes");
    assert_eq!(node(&first, "/vol/b").size_bytes, 5000);
    assert_eq!(node(&first, "/vol/b").file_count, 1);
    assert_eq!(node(&first, "/vol").size_bytes, 11_000);
}

#[test]
fn a_root_that_is_itself_excluded_is_still_walked_when_named() {
    // Naming an excluded folder is how the user asks to see inside it; the
    // exclusion applies to what is met *below* a root, never to the root.
    let options = WalkOptions { exclude: vec![PathBuf::from("/vol")], ..WalkOptions::default() };

    let result = walk_sample(&sample(), &options);

    assert!(!result.nodes.is_empty(), "the named root is walked");
    assert_eq!(node(&result, "/vol").size_bytes, 11_000);
    assert!(result.errors.is_empty(), "an exclusion is a choice, not a failure");
}

#[test]
fn only_as_many_files_as_asked_for_are_kept() {
    let options =
        WalkOptions { report_files_min_size: Some(0), report_files_top: 2, ..WalkOptions::default() };

    let result = walk_sample(&sample(), &options);

    assert!(result.files_truncated, "more files passed the threshold than were kept");
    let kept: Vec<u64> = result.files.iter().map(|file| file.size_bytes).collect();
    assert_eq!(kept.len(), 2);
    assert!(kept.contains(&5000) && kept.contains(&3000), "{kept:?}");
}

#[test]
fn progress_counts_every_entry_and_every_byte() {
    let clock = FixedClock::default();
    let sink = |_progress| {};
    let reporter = ProgressReporter::new(&sink, &clock);
    let options = WalkOptions { progress: Some(&reporter), ..WalkOptions::default() };

    let result = walk_sample(&sample(), &options);

    assert_eq!(reporter.snapshot().entries_scanned, 8);
    assert_eq!(reporter.snapshot().bytes_scanned, node(&result, "/vol").size_bytes);
}

#[test]
fn a_discounted_hard_link_is_not_a_directory_s_biggest_item() {
    // `/vol/mid` holds ten real 90-byte files and one name of a big file whose
    // other name, in `/vol/aaa`, wins the credit. The link must not pose as a
    // dominant item of `mid`: nothing there is bigger than one block.
    let fs = FakeFileOps::new().with_root("/vol", 1);
    fs.add_file("/vol/aaa/keeper.bin", &[]);
    fs.set_size("/vol/aaa/keeper.bin", 100_000);
    fs.add_hard_link("/vol/aaa/keeper.bin", "/vol/mid/alias.bin");
    for index in 0..10 {
        fs.add_file(format!("/vol/mid/real{index}.bin"), &[]);
        fs.set_size(format!("/vol/mid/real{index}.bin"), 90);
    }

    let result = walk_sample(&fs, &WalkOptions::default());

    let mid = node(&result, "/vol/mid");
    assert_eq!(mid.largest_item_bytes, 4096, "one 4 KiB block: the biggest *real* file");
    assert!(mid.largest_item_bytes * 2 < mid.allocated_bytes, "so nothing dominates `mid`");
    let aaa = node(&result, "/vol/aaa");
    assert_eq!(aaa.largest_item_bytes, aaa.allocated_bytes, "the credited name is the whole directory");
}

#[test]
fn a_directory_holding_only_a_discounted_link_has_no_item_at_all() {
    let fs = FakeFileOps::new().with_root("/vol", 1);
    fs.add_file("/vol/aaa/keeper.bin", &[]);
    fs.set_size("/vol/aaa/keeper.bin", 100_000);
    fs.add_hard_link("/vol/aaa/keeper.bin", "/vol/zzz/alias.bin");

    let result = walk_sample(&fs, &WalkOptions::default());

    let zzz = node(&result, "/vol/zzz");
    assert_eq!(zzz.allocated_bytes, 0, "the bytes were credited to `aaa`");
    assert_eq!(zzz.largest_item_bytes, 0);
}

#[test]
fn a_depth_limit_keeps_the_largest_item_of_what_it_hides() {
    let fs = FakeFileOps::new().with_root("/vol", 1);
    fs.add_file("/vol/a/b/c/huge.bin", &[]);
    fs.set_size("/vol/a/b/c/huge.bin", 900_000);
    fs.add_file("/vol/a/small.bin", &[]);
    fs.set_size("/vol/a/small.bin", 10);
    let options = WalkOptions { max_depth: Some(1), ..WalkOptions::default() };

    let limited = walk_sample(&fs, &options);
    let full = walk_sample(&fs, &WalkOptions::default());

    assert_eq!(
        node(&limited, "/vol/a").largest_item_bytes,
        node(&full, "/vol/a").largest_item_bytes,
        "`max_depth` limits what is reported, never what is measured"
    );
}

#[test]
fn a_depth_limit_does_not_resurrect_a_discounted_link_below_it() {
    let fs = FakeFileOps::new().with_root("/vol", 1);
    fs.add_file("/vol/aaa/keeper.bin", &[]);
    fs.set_size("/vol/aaa/keeper.bin", 1_000_000);
    fs.add_hard_link("/vol/aaa/keeper.bin", "/vol/mid/deep/alias.bin");
    for name in ["half1.bin", "half2.bin"] {
        fs.add_file(format!("/vol/mid/deep/{name}"), &[]);
        fs.set_size(format!("/vol/mid/deep/{name}"), 300_000);
    }
    let options = WalkOptions { max_depth: Some(1), ..WalkOptions::default() };

    let limited = walk_sample(&fs, &options);
    let full = walk_sample(&fs, &WalkOptions::default());

    assert_eq!(node(&limited, "/vol/mid").largest_item_bytes, node(&full, "/vol/mid").largest_item_bytes);
    assert!(node(&limited, "/vol/mid").largest_item_bytes <= node(&limited, "/vol/mid").allocated_bytes);
    assert!(limited.nodes.iter().all(|n| n.path != Path::new("/vol/mid/deep")), "hidden stays hidden");
}
