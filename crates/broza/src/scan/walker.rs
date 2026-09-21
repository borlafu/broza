//! Parallel directory walker over the [`FileOps`] port.
//!
//! One walk produces one [`DirNode`] per directory, carrying the aggregate of its
//! whole subtree, plus the biggest files. The walk is parallel (`rayon`), reads
//! the filesystem only through [`FileOps`] — so unit tests run against
//! `FakeFileOps` and never touch a real disk (`AGENTS.md` §7) — and never
//! aborts: an unreadable subtree becomes a warning and the rest of the tree is
//! still reported.
//!
//! # What the numbers mean
//!
//! - `size_bytes` is apparent size, `allocated_bytes` is what the filesystem
//!   really spends. Directory entries themselves contribute neither: only files
//!   and symlinks do, so a tree of empty directories is 0 bytes everywhere.
//! - A file with several hard links is counted **once per walk**, and always in
//!   the directory of the name that sorts first, so two runs agree
//!   (`dedupe.rs` settles that once the walk is done). **APFS clones are not
//!   deduplicated in v1**: two clones read
//!   as two ordinary files and are counted twice. That is best-effort work
//!   deferred to after 1.0 (PRD RF-02, `docs/cli-spec.md` §4.2), and it errs
//!   towards reporting more space in use than there is.
//! - A cloud placeholder (`SF_DATALESS`: iCloud Drive, Files On-Demand) counts
//!   as an entry and as zero bytes, because none of its bytes are on this disk.
//!   A dataless *directory* is not opened at all: doing so blocks on the
//!   provider (`docs/cli-spec.md` §7).
//! - Symlinks count as their own size and are never followed. There is no
//!   option to follow them: a cleanup tool that follows links walks out of the
//!   volume it was asked about.
//! - `max_depth` limits what is **reported**, never what is aggregated: a node
//!   at the depth limit still carries the bytes of everything below it and says
//!   so with `children_truncated`.
//!
//! # What the cache may answer
//!
//! The root is always walked, and the caller can hold the cache off to any
//! depth ([`WalkOptions::cache_from_depth`]): whatever the report shows has to
//! be measured this run, or a warm scan would print a tree with no branches.
//! Below that, an unchanged directory is taken from its [`DirRecord`] whole —
//! bytes, counts and all — and its subtree is not walked.

mod dedupe;
mod parts;
mod top_files;
mod types;

use std::path::Path;

use rayon::iter::{IntoParallelIterator, ParallelIterator};

pub use types::{
    DirIdentity, DirNode, FileEntry, MIN_CACHE_DEPTH, PERMISSION_DENIED_CODE, SkipHook,
    UNREADABLE_ENTRY_CODE, WalkOptions, WalkResult,
};

use crate::BrozaError;
use crate::ports::{EntryMetadata, FileOps};
use crate::scan::cache::DirRecord;
use parts::{Children, Context, Leaves, Partial, Totals};

/// Walk `root`, aggregating every directory below it.
///
/// Never fails: a root that cannot be read comes back as a single warning in
/// [`WalkResult::errors`], and so does every unreadable subtree found on the way.
pub fn walk(root: &Path, options: &WalkOptions<'_>, fs: &dyn FileOps) -> WalkResult {
    if options.exclude.iter().any(|prefix| root.starts_with(prefix)) {
        // Being asked to walk what one was told to leave out is not an error;
        // there is simply nothing to report.
        return WalkResult::default();
    }
    let meta = match fs.metadata(root) {
        Ok(meta) => meta,
        Err(error) => {
            return WalkResult { errors: vec![parts::diagnostic(root, &error)], ..WalkResult::default() };
        }
    };
    let context = Context { fs, options, root_device: meta.device };
    let partial = if meta.is_dir && !meta.is_symlink && !meta.is_dataless {
        walk_dir(root, &meta, 0, &context)
    } else {
        walk_leaf_root(root, &meta, &context)
    };
    sorted(partial)
}

/// Walk one directory and, in parallel, everything below it.
fn walk_dir(path: &Path, meta: &EntryMetadata, depth: usize, context: &Context<'_>) -> Partial {
    let identity = DirIdentity::of(path, meta);
    if let Some(record) = cached(&identity, depth, context) {
        return cached_dir(&identity, &record, context);
    }
    let listing = match context.fs.read_dir_with_metadata(path) {
        Ok(listing) => listing,
        Err(error) => return unreadable_dir(&identity, &error, context),
    };
    let Children { dirs, leaves, errors, skipped } = parts::split_children(listing, context);
    context.report(leaves.entries.saturating_add(1), leaves.totals.size_bytes);
    let child_dirs = dirs.len() as u64;
    let cap = context.options.report_files_top;
    let below = dirs
        .into_par_iter()
        .map(|(child, child_meta)| walk_dir(&child, &child_meta, depth.saturating_add(1), context))
        .reduce(|| Partial::empty(cap), Partial::merge);
    let reports_children = context.options.max_depth.is_none_or(|max| depth < max);
    let totals = leaves.totals.merge(below.totals).with_child_dirs(child_dirs);
    let node = DirNode::new(&identity, totals, skipped || !reports_children);
    let mut nodes = if reports_children { below.nodes } else { Vec::new() };
    nodes.push(node);
    let mut links = leaves.links;
    links.extend(below.links);
    let mut all_errors = errors;
    all_errors.extend(below.errors);
    Partial { nodes, files: leaves.files.merge(below.files), links, errors: all_errors, totals }
}

/// What the cache says about this directory, if it may speak at this depth.
fn cached(identity: &DirIdentity, depth: usize, context: &Context<'_>) -> Option<DirRecord> {
    if !context.options.cache_answers_at(depth) {
        return None;
    }
    context.options.skip_hook.and_then(|hook| hook(identity))
}

/// A directory served from the cache: measured last time, not walked again.
fn cached_dir(identity: &DirIdentity, record: &DirRecord, context: &Context<'_>) -> Partial {
    let totals = Totals {
        size_bytes: record.size_bytes,
        allocated_bytes: record.allocated_bytes,
        file_count: record.file_count,
        dir_count: record.dir_count,
        dataless_count: record.dataless_count,
        largest_item_bytes: record.largest_item_bytes,
    };
    context.report(record.file_count.saturating_add(record.dir_count).saturating_add(1), record.size_bytes);
    Partial {
        nodes: vec![DirNode::new(identity, totals, false)],
        totals,
        ..Partial::empty(context.options.report_files_top)
    }
}

/// A directory Broza may not read: a warning, an empty node, and the walk goes on.
fn unreadable_dir(identity: &DirIdentity, error: &BrozaError, context: &Context<'_>) -> Partial {
    context.report(1, 0);
    Partial {
        nodes: vec![parts::truncated_node(identity, Totals::default())],
        errors: vec![parts::diagnostic(&identity.path, error)],
        ..Partial::empty(context.options.report_files_top)
    }
}

/// A root that is not a directory: reported as a file, with no node.
fn walk_leaf_root(root: &Path, meta: &EntryMetadata, context: &Context<'_>) -> Partial {
    let cap = context.options.report_files_top;
    let leaves = Leaves::empty(cap).add_leaf(root, meta, context);
    context.report(leaves.entries, leaves.totals.size_bytes);
    Partial { files: leaves.files, totals: leaves.totals, ..Partial::empty(cap) }
}

/// Settle the hard links and put everything in a deterministic order.
///
/// Parallel walks finish in whatever order the threads happen to take, and a report
/// that changes between two identical scans is a report nobody can diff.
fn sorted(partial: Partial) -> WalkResult {
    let Partial { nodes, files, links, mut errors, .. } = partial;
    let (mut nodes, mut files) = dedupe::discount_duplicates(nodes, files.into_vec(), links);
    nodes.sort_by(|left, right| left.path.cmp(&right.path));
    files.sort_by(|left, right| left.path.cmp(&right.path));
    errors.sort_by(|left, right| left.path.cmp(&right.path).then_with(|| left.code.cmp(&right.code)));
    WalkResult { nodes, files, errors }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use jiff::Timestamp;

    use super::{DirIdentity, DirNode, WalkOptions, WalkResult, walk};
    use crate::scan::cache::{CacheKey, DirRecord};
    use crate::scan::progress::ProgressReporter;
    use crate::testing::{FakeFileOps, FixedClock};

    /// How many files a test keeps, when it asks for files at all.
    const KEEP_FILES: usize = 10;

    /// A cache record standing for a subtree of `size_bytes`.
    fn record(identity: &DirIdentity, size_bytes: u64) -> DirRecord {
        DirRecord {
            key: CacheKey::of(identity).unwrap_or_else(|| panic!("no key for {identity:?}")),
            size_bytes,
            allocated_bytes: size_bytes * 2,
            file_count: 3,
            dir_count: 1,
            dataless_count: 0,
            largest_item_bytes: size_bytes,
            recorded_at: Timestamp::UNIX_EPOCH,
        }
    }

    /// Apparent size of every file of the sample tree, by path.
    const SAMPLE: [(&str, u64); 4] =
        [("/vol/a/f1", 1000), ("/vol/a/f2", 2000), ("/vol/a/sub/f3", 3000), ("/vol/b/big", 5000)];

    fn sample() -> FakeFileOps {
        SAMPLE
            .iter()
            .fold(FakeFileOps::new().with_root("/vol", 1), |fs, (path, size)| fs.with_sized_file(path, *size))
    }

    fn node<'a>(result: &'a WalkResult, path: &str) -> &'a DirNode {
        result
            .nodes
            .iter()
            .find(|node| node.path == Path::new(path))
            .unwrap_or_else(|| panic!("no node for {path} in {:?}", result.paths()))
    }

    fn walk_sample(fs: &FakeFileOps, options: &WalkOptions<'_>) -> WalkResult {
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

        assert_eq!(
            result.paths(),
            vec![PathBuf::from("/vol"), PathBuf::from("/vol/a"), PathBuf::from("/vol/b")]
        );
        assert_eq!(node(&result, "/vol").size_bytes, 11_000);
        assert_eq!(node(&result, "/vol/a").size_bytes, 6000);
        assert!(node(&result, "/vol/a").children_truncated);
        assert!(!node(&result, "/vol").children_truncated);
    }

    #[test]
    fn a_cache_hit_reuses_the_whole_aggregate_and_does_not_descend() {
        let seen: Mutex<Vec<DirIdentity>> = Mutex::new(Vec::new());
        let hook = |identity: &DirIdentity| {
            seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(identity.clone());
            (identity.path == Path::new("/vol/a")).then(|| record(identity, 777))
        };
        let options = WalkOptions { skip_hook: Some(&hook), ..WalkOptions::default() };

        let result = walk_sample(&sample(), &options);

        assert_eq!(
            result.paths(),
            vec![PathBuf::from("/vol"), PathBuf::from("/vol/a"), PathBuf::from("/vol/b")]
        );
        let cached = node(&result, "/vol/a");
        assert_eq!(cached.size_bytes, 777);
        assert_eq!(cached.allocated_bytes, 1554);
        assert_eq!(cached.file_count, 3);
        assert_eq!(cached.dir_count, 1);
        assert!(!cached.children_truncated, "a cached subtree was measured, not truncated");
        let root = node(&result, "/vol");
        assert_eq!(root.size_bytes, 5777);
        assert_eq!(root.file_count, 4, "three cached files plus the one under /vol/b");
        assert_eq!(root.dir_count, 3, "a and b, plus the one inside a");
        let asked = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(asked.iter().all(|identity| identity.device == 1));
        assert!(asked.iter().any(|identity| identity.path == Path::new("/vol/a")));
    }

    #[test]
    fn the_cache_is_never_asked_about_the_root_itself() {
        let hook = |identity: &DirIdentity| Some(record(identity, 1));
        let options = WalkOptions { skip_hook: Some(&hook), ..WalkOptions::default() };

        let result = walk_sample(&sample(), &options);

        // Answering at the root would report a volume with no contents at all.
        assert_eq!(node(&result, "/vol").size_bytes, 2, "both children came from the cache");
        assert_eq!(result.paths().len(), 3);
    }

    #[test]
    fn the_cache_can_be_held_off_until_a_deeper_level() {
        let hook = |identity: &DirIdentity| Some(record(identity, 1));
        let options = WalkOptions { skip_hook: Some(&hook), cache_from_depth: 2, ..WalkOptions::default() };

        let result = walk_sample(&sample(), &options);

        assert_eq!(
            result.paths(),
            vec![
                PathBuf::from("/vol"),
                PathBuf::from("/vol/a"),
                PathBuf::from("/vol/a/sub"),
                PathBuf::from("/vol/b"),
            ],
            "the first two levels are walked whatever the cache says"
        );
        assert_eq!(node(&result, "/vol/a/sub").size_bytes, 1, "the third level is cached");
        assert_eq!(node(&result, "/vol/a").size_bytes, 3001, "its own files, plus the cached subtree");
    }

    #[test]
    fn files_are_collected_only_above_the_reporting_threshold() {
        let options = WalkOptions {
            report_files_min_size: Some(2500),
            report_files_top: KEEP_FILES,
            ..WalkOptions::default()
        };

        let result = walk_sample(&sample(), &options);

        let collected: Vec<_> =
            result.files.iter().map(|file| (file.path.clone(), file.size_bytes)).collect();
        assert_eq!(
            collected,
            vec![(PathBuf::from("/vol/a/sub/f3"), 3000), (PathBuf::from("/vol/b/big"), 5000)]
        );
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
        let options = WalkOptions {
            max_depth: Some(3),
            exclude: vec![PathBuf::from("/vol/a")],
            ..WalkOptions::default()
        };

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
    fn a_root_that_is_itself_excluded_reports_nothing() {
        let options = WalkOptions { exclude: vec![PathBuf::from("/vol")], ..WalkOptions::default() };

        let result = walk_sample(&sample(), &options);

        assert!(result.nodes.is_empty(), "{:?}", result.paths());
        assert!(result.files.is_empty());
        assert!(result.errors.is_empty(), "an exclusion is a choice, not a failure");
    }

    #[test]
    fn only_as_many_files_as_asked_for_are_kept() {
        let options =
            WalkOptions { report_files_min_size: Some(0), report_files_top: 2, ..WalkOptions::default() };

        let result = walk_sample(&sample(), &options);

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
}
