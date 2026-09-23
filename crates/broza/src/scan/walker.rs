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
//!   the directory of the name that sorts first, so two runs agree (`dedupe.rs`
//!   settles that once the walk is done). An APFS clone family is counted once
//!   too, where its original stands or, failing that, at its first path
//!   (`clones.rs`); a clone that has diverged from its original is discounted
//!   whole, which is the best the clone id allows (PRD RF-02,
//!   `docs/cli-spec.md` §4.2).
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
//! Below that, an unchanged directory is taken from its [`crate::scan::DirRecord`] whole —
//! bytes, counts and all — and its subtree is not walked.

mod clones;
mod dedupe;
mod parts;
mod top_files;
mod types;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use rayon::iter::{IntoParallelIterator, ParallelIterator};

pub use types::{
    CachedSubtree, DirIdentity, DirNode, FileEntry, MIN_CACHE_DEPTH, PERMISSION_DENIED_CODE, SkipHook,
    UNREADABLE_ENTRY_CODE, WalkOptions, WalkResult,
};

use crate::BrozaError;
use crate::ports::{EntryMetadata, FileOps};
use parts::{Children, Context, Leaves, Partial, Totals};
use top_files::TopFiles;

/// Stack of each walker thread.
///
/// The walk recurses once per directory level, and a macOS path holds up to
/// `PATH_MAX / 2` levels (512). The two megabytes a thread gets by default ran
/// out at 466 levels once the frames grew, and a walker that aborts on a deep
/// tree reports nothing at all. Reserved, not committed: the pages are only
/// touched as deep as the tree goes.
const WALK_STACK_BYTES: usize = 64 * 1024 * 1024;

/// The pool every walk runs on, built once.
///
/// `None` when the pool could not be built, in which case the walk runs on
/// whatever pool the caller is on: slower to fail on a deep tree, never wrong.
fn pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| rayon::ThreadPoolBuilder::new().stack_size(WALK_STACK_BYTES).build().ok()).as_ref()
}

/// Walk `root`, aggregating every directory below it.
///
/// Never fails: a root that cannot be read comes back as a single warning in
/// [`WalkResult::errors`], and so does every unreadable subtree found on the way.
pub fn walk(root: &Path, options: &WalkOptions<'_>, fs: &dyn FileOps) -> WalkResult {
    match pool() {
        Some(pool) => pool.install(|| walk_on_this_pool(root, options, fs)),
        None => walk_on_this_pool(root, options, fs),
    }
}

/// [`walk`], on whatever pool the caller is on.
fn walk_on_this_pool(root: &Path, options: &WalkOptions<'_>, fs: &dyn FileOps) -> WalkResult {
    // The root is never subject to the exclusions: naming an excluded folder
    // explicitly is how the user asks to see inside it (`docs/cli-spec.md` §7).
    // Exclusions apply to what is *met* below the root.
    let meta = match fs.metadata(root) {
        Ok(meta) => meta,
        Err(error) => {
            return WalkResult { errors: vec![parts::diagnostic(root, &error)], ..WalkResult::default() };
        }
    };
    let exclude = options.exclude.iter().filter(|prefix| !root.starts_with(prefix)).cloned().collect();
    let context = Context { fs, options, root_device: meta.device, exclude };
    let partial = if meta.is_dir && !meta.is_symlink && !meta.is_dataless {
        walk_dir(root, &meta, 0, &context)
    } else {
        walk_leaf_root(root, &meta, &context)
    };
    sorted(partial)
}

/// Walk one directory and, in parallel, everything below it.
///
/// The totals handed back to the caller count this subtree as an item in its
/// own right ([`Totals::as_child`]); the node keeps the view from inside.
fn walk_dir(path: &Path, meta: &EntryMetadata, depth: usize, context: &Context<'_>) -> Partial {
    let identity = DirIdentity::of(path, meta);
    if let Some(subtree) = cached(&identity, depth, context).filter(|subtree| !subtree.nodes.is_empty()) {
        return cached_dir(&identity, subtree, depth, context);
    }
    let listing = match context.fs.read_dir_with_metadata(path) {
        Ok(listing) => listing,
        Err(error) => return unreadable_dir(&identity, &error, context),
    };
    let Children { dirs, leaves, errors, skipped, .. } = parts::split_children(path, listing, context);
    context.report(leaves.entries.saturating_add(1), leaves.totals.size_bytes);
    let child_dirs = dirs.len() as u64;
    let cap = context.options.report_files_top;
    let below = dirs
        .into_par_iter()
        .map(|(child, child_meta)| walk_dir(&child, &child_meta, depth.saturating_add(1), context))
        .reduce(|| Partial::empty(cap), Partial::merge);
    let reports_children = context.options.max_depth.is_none_or(|max| depth < max);
    let truncated = skipped || !reports_children;
    let totals = leaves.totals.merge(below.totals).with_child_dirs(child_dirs).with_truncation(truncated);
    let node = DirNode::new(&identity, totals, truncated);
    // Children below `max_depth` are not reported, but they are still this
    // directory's items: the limit shapes what is reported, never what is
    // measured. They travel as `hidden`, get settled and recomputed with
    // everything else, and are dropped from the report at the very end.
    let mut hidden = below.hidden;
    let mut nodes = if reports_children {
        below.nodes
    } else {
        hidden.extend(below.nodes);
        Vec::new()
    };
    nodes.push(node);
    let mut links = leaves.links;
    links.extend(below.links);
    let mut all_errors = errors;
    all_errors.extend(below.errors);
    let mut direct_maxima = below.direct_maxima;
    direct_maxima.push((identity.path.clone(), totals.largest_direct_file_bytes));
    let mut cache_files = leaves.cache_files;
    cache_files.extend(below.cache_files);
    Partial {
        nodes,
        files: leaves.files.merge(below.files),
        links,
        errors: all_errors,
        totals: totals.as_child(),
        direct_maxima,
        hidden,
        cache_files,
        originals: leaves.originals.merge(below.originals),
        clones: leaves.clones.merge(below.clones),
    }
}

/// What the cache says about this directory, if it may speak at this depth.
fn cached(identity: &DirIdentity, depth: usize, context: &Context<'_>) -> Option<CachedSubtree> {
    if !context.options.cache_answers_at(depth) {
        return None;
    }
    context.options.skip_hook.and_then(|hook| hook(identity))
}

/// A subtree served from the cache: measured last time, not walked again.
///
/// Its nodes come back as the cache kept them (those below `max_depth` hidden,
/// like walked ones), its files above each floor are collected like walked
/// ones, and the aggregate of its root is what the parent adds up.
fn cached_dir(
    identity: &DirIdentity,
    subtree: CachedSubtree,
    depth: usize,
    context: &Context<'_>,
) -> Partial {
    let CachedSubtree { nodes, files, kept_clones } = subtree;
    let Some(root) = nodes.first() else {
        return Partial::empty(context.options.report_files_top);
    };
    // A family whose credited clone lives in this subtree is spoken for: any
    // other clone of it the walk meets is discounted, as the cold walk that
    // wrote the record did — so the family reads as one with a known original.
    let originals =
        kept_clones.iter().fold(clones::Originals::default(), |mut originals, (device, inode)| {
            originals.record(*device, *inode);
            originals
        });
    let totals = Totals {
        size_bytes: root.size_bytes,
        allocated_bytes: root.allocated_bytes,
        file_count: root.file_count,
        dir_count: root.dir_count,
        dataless_count: root.dataless_count,
        largest_item_bytes: root.largest_item_bytes,
        largest_direct_file_bytes: 0,
        has_truncation: root.has_truncation,
    };
    context.report(root.file_count.saturating_add(root.dir_count).saturating_add(1), root.size_bytes);
    let root_depth = identity.path.components().count();
    let reported = |node: &DirNode| {
        let below_root = node.path.components().count().saturating_sub(root_depth);
        context.options.max_depth.is_none_or(|max| depth.saturating_add(below_root) <= max)
    };
    let (nodes, hidden): (Vec<DirNode>, Vec<DirNode>) = nodes.into_iter().partition(reported);
    // The served files are already in the store under the records they came
    // from, so none is collected for it again; only the report wants them.
    let cap = context.options.report_files_top;
    let top = files
        .into_iter()
        .filter(|file| {
            let reportable = file.size_bytes.max(file.allocated_bytes);
            context.options.report_files_min_size.is_some_and(|min| reportable >= min)
        })
        .fold(TopFiles::new(cap), TopFiles::with);
    Partial { nodes, hidden, files: top, totals: totals.as_child(), originals, ..Partial::empty(cap) }
}

/// A directory Broza may not read: a warning, an empty node, and the walk goes on.
fn unreadable_dir(identity: &DirIdentity, error: &BrozaError, context: &Context<'_>) -> Partial {
    context.report(1, 0);
    let totals = Totals::default().with_truncation(true);
    Partial {
        nodes: vec![parts::truncated_node(identity, totals)],
        totals,
        errors: vec![parts::diagnostic(&identity.path, error)],
        ..Partial::empty(context.options.report_files_top)
    }
}

/// A root that is not a directory: reported as a file, with no node.
fn walk_leaf_root(root: &Path, meta: &EntryMetadata, context: &Context<'_>) -> Partial {
    let cap = context.options.report_files_top;
    let dir: Arc<Path> = Arc::from(root.parent().unwrap_or(root));
    let leaves = Leaves::empty(cap).add_leaf(&dir, root.to_path_buf(), meta, context);
    context.report(leaves.entries, leaves.totals.size_bytes);
    Partial {
        files: leaves.files,
        cache_files: leaves.cache_files,
        totals: leaves.totals,
        originals: leaves.originals,
        clones: leaves.clones,
        ..Partial::empty(cap)
    }
}

/// Recompute every directory's largest item from what the settled tree holds.
///
/// The walk measures the largest item before hard links are settled, so a file
/// counted under several names could leave an ancestor claiming more than it
/// holds — and a warm scan, working from the settled records, would disagree
/// with the cold one that produced them. Bottom-up, each directory's largest
/// item becomes the biggest of its own files and of its children (a child's
/// whole subtree is an item), never more than the directory itself. Directories
/// served from the cache keep the value their record carries and only feed
/// their parents.
fn recompute_largest_items(mut nodes: Vec<DirNode>, direct_maxima: &[(PathBuf, u64)]) -> Vec<DirNode> {
    let mut direct: HashMap<&Path, u64> = HashMap::new();
    for (path, max) in direct_maxima {
        let entry = direct.entry(path.as_path()).or_insert(0);
        *entry = (*entry).max(*max);
    }
    let mut from_children: HashMap<PathBuf, u64> = HashMap::new();
    nodes.sort_by_cached_key(|node| std::cmp::Reverse(node.path.components().count()));
    for node in &mut nodes {
        if !node.from_cache {
            let own = direct.get(node.path.as_path()).copied().unwrap_or(0);
            let below = from_children.get(node.path.as_path()).copied().unwrap_or(0);
            node.largest_item_bytes = own.max(below).min(node.allocated_bytes);
        }
        if let Some(parent) = node.path.parent() {
            let as_item = node.largest_item_bytes.max(node.allocated_bytes);
            match from_children.get_mut(parent) {
                Some(entry) => *entry = (*entry).max(as_item),
                None => {
                    from_children.insert(parent.to_path_buf(), as_item);
                }
            }
        }
    }
    nodes
}

/// Settle the shared bytes and put everything in a deterministic order.
///
/// Parallel walks finish in whatever order the threads happen to take, and a report
/// that changes between two identical scans is a report nobody can diff.
fn sorted(partial: Partial) -> WalkResult {
    let Partial {
        mut nodes,
        files,
        links,
        mut errors,
        mut direct_maxima,
        hidden,
        mut cache_files,
        originals,
        clones: ledger,
        ..
    } = partial;
    let hidden_paths: HashSet<PathBuf> = hidden.iter().map(|node| node.path.clone()).collect();
    nodes.extend(hidden);
    let files_truncated = files.is_truncated();
    let settled = dedupe::settle_hard_links(nodes, files.into_vec(), links);
    let dropped: HashSet<&Path> = settled.dropped.iter().map(PathBuf::as_path).collect();
    cache_files.retain(|file| !dropped.contains(file.path.as_path()));
    let clones::Settlement { nodes, mut files, mut cache_files, credited, mut kept_clones } =
        clones::settle_clones(settled.nodes, settled.files, cache_files, ledger, originals);
    // The surviving name of every multiply-linked file or clone is a direct
    // file of the directory it is credited to; the discounted names count
    // nowhere.
    for link in settled.credited.iter().chain(&credited) {
        if let Some(parent) = link.path.parent() {
            direct_maxima.push((parent.to_path_buf(), link.allocated_bytes));
        }
    }
    let mut nodes = recompute_largest_items(nodes, &direct_maxima);
    nodes.retain(|node| !hidden_paths.contains(&node.path));
    nodes.sort_by(|left, right| left.path.cmp(&right.path));
    files.sort_by(|left, right| left.path.cmp(&right.path));
    cache_files.sort_by(|left, right| left.path.cmp(&right.path));
    errors.sort_by(|left, right| left.path.cmp(&right.path).then_with(|| left.code.cmp(&right.code)));
    kept_clones.sort_unstable();
    WalkResult { nodes, files, files_truncated, cache_files, errors, kept_clones }
}

#[cfg(test)]
mod cache_tests;
#[cfg(test)]
mod clone_tests;
#[cfg(test)]
mod tests;
