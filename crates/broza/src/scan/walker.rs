//! Parallel directory walker over the [`FileOps`] port.
//!
//! One walk produces one [`DirNode`] per directory, carrying the aggregate of its
//! whole subtree, plus the files big enough to be worth reporting. The walk is
//! parallel (`rayon`), reads the filesystem only through [`FileOps`] — so unit
//! tests run against `FakeFileOps` and never touch a real disk (`AGENTS.md` §7) —
//! and never aborts: an unreadable subtree becomes a warning and the rest of the
//! tree is still reported.
//!
//! # What the numbers mean
//!
//! - `size_bytes` is apparent size, `allocated_bytes` is what the filesystem really
//!   spends. Directory entries themselves contribute neither: only files and
//!   symlinks do, so a tree of empty directories is 0 bytes on every filesystem.
//! - A file with several hard links is counted **once per walk**
//!   (`docs/cli-spec.md` §4.2). Which directory gets the bytes is whichever thread
//!   reaches a name first, so the total is stable but the attribution between two
//!   directories holding names of the same inode is not.
//! - Symlinks count as their own size and are never followed. There is no option
//!   to follow them: a cleanup tool that follows links walks out of the volume it
//!   was asked about.
//! - `max_depth` limits what is **reported**, never what is aggregated: a node at
//!   the depth limit still carries the bytes of everything below it and says so
//!   with `children_truncated`.

mod parts;
mod types;

use std::path::Path;

use rayon::iter::{IntoParallelIterator, ParallelIterator};

pub use types::{
    DirIdentity, DirNode, FileEntry, PERMISSION_DENIED_CODE, SkipHook, UNREADABLE_ENTRY_CODE, WalkOptions,
    WalkResult,
};

use crate::BrozaError;
use crate::ports::{EntryMetadata, FileOps};
use crate::scan::links::LinkRegistry;
use parts::{Children, Context, Leaves, Partial, Totals};

/// Walk `root`, aggregating every directory below it.
///
/// Never fails: a root that cannot be read comes back as a single warning in
/// [`WalkResult::errors`], and so does every unreadable subtree found on the way.
pub fn walk(root: &Path, options: &WalkOptions<'_>, fs: &dyn FileOps) -> WalkResult {
    let meta = match fs.metadata(root) {
        Ok(meta) => meta,
        Err(error) => {
            return WalkResult { errors: vec![parts::diagnostic(root, &error)], ..WalkResult::default() };
        }
    };
    let context = Context { fs, options, links: LinkRegistry::new(), root_device: meta.device };
    let partial = if meta.is_dir && !meta.is_symlink {
        walk_dir(root, &meta, 0, &context)
    } else {
        walk_leaf_root(root, &meta, &context)
    };
    sorted(partial)
}

/// Walk one directory and, in parallel, everything below it.
fn walk_dir(path: &Path, meta: &EntryMetadata, depth: usize, context: &Context<'_>) -> Partial {
    let identity = DirIdentity::of(path, meta);
    if let Some(size_bytes) = context.options.skip_hook.and_then(|hook| hook(&identity)) {
        return cached_dir(&identity, size_bytes, context);
    }
    let children = match context.fs.read_dir(path) {
        Ok(children) => children,
        Err(error) => return unreadable_dir(&identity, &error, context),
    };
    let Children { dirs, leaves, errors, skipped } = parts::split_children(children, context);
    context.report(leaves.entries.saturating_add(1), leaves.totals.size_bytes);
    let child_dirs = dirs.len() as u64;
    let below = dirs
        .into_par_iter()
        .map(|(child, child_meta)| walk_dir(&child, &child_meta, depth.saturating_add(1), context))
        .reduce(Partial::default, Partial::merge);
    let reports_children = context.options.max_depth.is_none_or(|max| depth < max);
    let totals = leaves.totals.merge(below.totals).with_child_dirs(child_dirs);
    let node = DirNode::new(&identity, totals, skipped || !reports_children);
    let mut nodes = if reports_children { below.nodes } else { Vec::new() };
    nodes.push(node);
    let mut files = leaves.files;
    files.extend(below.files);
    let mut all_errors = errors;
    all_errors.extend(below.errors);
    Partial { nodes, files, errors: all_errors, totals }
}

/// A directory served from the cache: its size is reused and nothing is walked.
fn cached_dir(identity: &DirIdentity, size_bytes: u64, context: &Context<'_>) -> Partial {
    context.report(1, size_bytes);
    let totals = Totals { size_bytes, ..Totals::default() };
    Partial { nodes: vec![parts::truncated_node(identity, totals)], totals, ..Partial::default() }
}

/// A directory Broza may not read: a warning, an empty node, and the walk goes on.
fn unreadable_dir(identity: &DirIdentity, error: &BrozaError, context: &Context<'_>) -> Partial {
    context.report(1, 0);
    Partial {
        nodes: vec![parts::truncated_node(identity, Totals::default())],
        errors: vec![parts::diagnostic(&identity.path, error)],
        ..Partial::default()
    }
}

/// A root that is not a directory: reported as a file, with no node.
fn walk_leaf_root(root: &Path, meta: &EntryMetadata, context: &Context<'_>) -> Partial {
    let leaves = Leaves::default().add_leaf(root, meta, context);
    context.report(leaves.entries, leaves.totals.size_bytes);
    Partial { files: leaves.files, totals: leaves.totals, ..Partial::default() }
}

/// Put nodes, files, and warnings in a deterministic order.
///
/// Parallel walks finish in whatever order the threads happen to take, and a report
/// that changes between two identical scans is a report nobody can diff.
fn sorted(partial: Partial) -> WalkResult {
    let Partial { mut nodes, mut files, mut errors, .. } = partial;
    nodes.sort_by(|left, right| left.path.cmp(&right.path));
    files.sort_by(|left, right| left.path.cmp(&right.path));
    errors.sort_by(|left, right| left.path.cmp(&right.path).then_with(|| left.code.cmp(&right.code)));
    WalkResult { nodes, files, errors }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use super::{DirIdentity, DirNode, WalkOptions, WalkResult, walk};
    use crate::scan::progress::ProgressReporter;
    use crate::testing::{FakeFileOps, FixedClock};

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
    fn a_cache_hit_reuses_the_subtree_size_and_does_not_descend() {
        let seen: Mutex<Vec<DirIdentity>> = Mutex::new(Vec::new());
        let hook = |identity: &DirIdentity| {
            seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(identity.clone());
            (identity.path == Path::new("/vol/a")).then_some(777)
        };
        let options = WalkOptions { skip_hook: Some(&hook), ..WalkOptions::default() };

        let result = walk_sample(&sample(), &options);

        assert_eq!(
            result.paths(),
            vec![PathBuf::from("/vol"), PathBuf::from("/vol/a"), PathBuf::from("/vol/b")]
        );
        assert_eq!(node(&result, "/vol/a").size_bytes, 777);
        assert!(node(&result, "/vol/a").children_truncated);
        assert_eq!(node(&result, "/vol").size_bytes, 5777);
        let asked = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(asked.iter().all(|identity| identity.device == 1));
        assert!(asked.iter().any(|identity| identity.path == Path::new("/vol/a")));
    }

    #[test]
    fn files_are_collected_only_above_the_reporting_threshold() {
        let options = WalkOptions { report_files_min_size: Some(2500), ..WalkOptions::default() };

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
        let options = WalkOptions { report_files_min_size: Some(0), ..WalkOptions::default() };

        let result = walk(Path::new("/vol/b/big"), &options, &sample());

        assert!(result.nodes.is_empty());
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].size_bytes, 5000);
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
