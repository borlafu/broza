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
//!   settles that once the walk is done). **APFS clones are not deduplicated in
//!   v1**: two clones read as two ordinary files and are counted twice. That is
//!   best-effort work deferred to after 1.0 (PRD RF-02, `docs/cli-spec.md`
//!   §4.2), and it errs towards reporting more space in use than there is.
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
mod tests;
