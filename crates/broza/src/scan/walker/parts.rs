//! Internals of the [walker](super): shared context, running totals, child triage.
//!
//! Split out of `walker.rs` so that both files stay small; nothing here is part of
//! the public surface.

use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::model::Diagnostic;
use crate::ports::{EntryMetadata, FileOps};
use crate::scan::walker::top_files::TopFiles;
use crate::scan::walker::{
    DirIdentity, DirNode, FileEntry, PERMISSION_DENIED_CODE, UNREADABLE_ENTRY_CODE, WalkOptions,
};

/// Everything one walk shares across its threads.
pub(super) struct Context<'a> {
    /// Filesystem the walk reads through.
    pub fs: &'a dyn FileOps,
    /// Options the caller asked for.
    pub options: &'a WalkOptions<'a>,
    /// Device of the root; the walk never leaves it unless asked to.
    pub root_device: u64,
    /// The exclusions that apply below the root: a prefix that covers the root
    /// itself is dropped, because naming the root was asking to see inside it.
    pub exclude: Vec<PathBuf>,
}

impl Context<'_> {
    /// Forward `entries` and `bytes` to the progress callback, when there is one.
    pub fn report(&self, entries: u64, bytes: u64) {
        if let Some(progress) = self.options.progress {
            progress.record(entries, bytes);
        }
    }

    /// `true` when `path` is under one of the excluded prefixes.
    pub fn is_excluded(&self, path: &Path) -> bool {
        self.exclude.iter().any(|prefix| path.starts_with(prefix))
    }

    /// `true` when following this entry would leave the root's device.
    pub fn crosses_device(&self, meta: &EntryMetadata) -> bool {
        self.options.same_device_only && meta.device != self.root_device
    }
}

/// One name of a file that has several of them.
///
/// The bytes of such a file must count once per walk, and *which* directory gets
/// them has to be the same on every run — so the walk records every sighting and
/// the choice is made afterwards, in [`super::dedupe`], by path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LinkSighting {
    /// Device of the inode.
    pub device: u64,
    /// The inode all the names share.
    pub inode: u64,
    /// This name.
    pub path: PathBuf,
    /// How many names the inode has in total, as the filesystem reports it.
    pub link_count: u64,
    /// Apparent bytes counted for it.
    pub size_bytes: u64,
    /// Allocated bytes counted for it.
    pub allocated_bytes: u64,
}

/// Aggregate of one subtree, without the identity of the directory it belongs to.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Totals {
    /// Apparent bytes of every file and symlink counted.
    pub size_bytes: u64,
    /// Allocated bytes of the same entries.
    pub allocated_bytes: u64,
    /// Non-directory entries counted.
    pub file_count: u64,
    /// Directories below, excluding the directory these totals describe.
    pub dir_count: u64,
    /// Cloud placeholders seen, whose bytes are not on this disk.
    pub dataless_count: u64,
    /// Biggest single reportable thing inside: the largest file, or the largest
    /// descendant directory's subtree. The cache uses it to know whether
    /// skipping this subtree could hide an entry the report would have shown.
    pub largest_item_bytes: u64,
    /// Biggest file *directly* in this directory, allocated bytes. Children add
    /// nothing here; it is what lets the largest item be recomputed once hard
    /// links are settled, without knowing every file.
    pub largest_direct_file_bytes: u64,
    /// `true` when something inside was not walked: an unreadable directory, a
    /// cloud placeholder, another device, an exclusion, a depth limit.
    ///
    /// Such a subtree cannot be served from the cache either. Its aggregate is
    /// incomplete by construction, and the warnings that explain why are
    /// produced by walking — a warm scan that skipped it would report the same
    /// bytes with none of the reasons.
    pub has_truncation: bool,
}

impl Totals {
    /// Totals of a single non-directory entry.
    pub fn leaf(meta: &EntryMetadata) -> Self {
        if meta.is_dataless {
            return Self { file_count: 1, dataless_count: 1, has_truncation: true, ..Self::default() };
        }
        Self {
            size_bytes: meta.size_bytes,
            allocated_bytes: meta.allocated_bytes,
            file_count: 1,
            dir_count: 0,
            dataless_count: 0,
            // Whichever size is bigger: a compressed file is reportable by its
            // apparent size, and the cache gate compares this against the
            // report's floor, so it must not undercount what could be listed.
            largest_item_bytes: meta.allocated_bytes.max(meta.size_bytes),
            // A file with several names may be credited to another directory
            // once hard links are settled; its size comes back through the
            // surviving sighting, so it must not be claimed twice here.
            largest_direct_file_bytes: if meta.link_count > 1 { 0 } else { meta.allocated_bytes },
            has_truncation: false,
        }
    }

    /// Sum of two subtrees, saturating instead of wrapping.
    pub fn merge(self, other: Self) -> Self {
        Self {
            size_bytes: self.size_bytes.saturating_add(other.size_bytes),
            allocated_bytes: self.allocated_bytes.saturating_add(other.allocated_bytes),
            file_count: self.file_count.saturating_add(other.file_count),
            dir_count: self.dir_count.saturating_add(other.dir_count),
            dataless_count: self.dataless_count.saturating_add(other.dataless_count),
            largest_item_bytes: self.largest_item_bytes.max(other.largest_item_bytes),
            largest_direct_file_bytes: self.largest_direct_file_bytes.max(other.largest_direct_file_bytes),
            has_truncation: self.has_truncation || other.has_truncation,
        }
    }

    /// The same totals, plus the directories that are direct children.
    pub fn with_child_dirs(self, count: u64) -> Self {
        Self { dir_count: self.dir_count.saturating_add(count), ..self }
    }

    /// The same totals, knowing something inside was not walked.
    pub fn with_truncation(self, truncated: bool) -> Self {
        Self { has_truncation: self.has_truncation || truncated, ..self }
    }

    /// The same totals as the directory *above* sees them.
    ///
    /// A subtree is itself something the report can list, and a big directory
    /// of small files is the ordinary case: ten megabytes in ten files is a
    /// ten-megabyte directory. Without this the cache would skip a parent
    /// whose child was about to be listed, and a warm scan would quietly lose
    /// that line.
    pub fn as_child(self) -> Self {
        Self {
            largest_item_bytes: self.largest_item_bytes.max(self.allocated_bytes),
            // A child's files are not the parent's direct files.
            largest_direct_file_bytes: 0,
            ..self
        }
    }
}

/// What one recursion step contributes to the walk.
#[derive(Debug)]
pub(super) struct Partial {
    /// Directory nodes worth reporting.
    pub nodes: Vec<DirNode>,
    /// The biggest files seen, bounded by the caller's limit.
    pub files: TopFiles,
    /// Names of files that have more than one.
    pub links: Vec<LinkSighting>,
    /// Warnings collected on the way.
    pub errors: Vec<Diagnostic>,
    /// Aggregate of the subtree.
    pub totals: Totals,
    /// Per reported directory, the biggest file directly inside it: what the
    /// largest item is recomputed from once hard links are settled.
    pub direct_maxima: Vec<(PathBuf, u64)>,
    /// Directories below `max_depth`: measured and settled like the rest, so the
    /// nodes above them are computed from the truth, then left out of the report.
    pub hidden: Vec<DirNode>,
    /// Every file above the cache's floor, unbounded, for the scan cache.
    pub cache_files: Vec<FileEntry>,
}

impl Partial {
    /// An empty contribution that keeps at most `cap` files.
    pub fn empty(cap: usize) -> Self {
        Self {
            nodes: Vec::new(),
            files: TopFiles::new(cap),
            links: Vec::new(),
            errors: Vec::new(),
            totals: Totals::default(),
            direct_maxima: Vec::new(),
            hidden: Vec::new(),
            cache_files: Vec::new(),
        }
    }

    /// Concatenate two partial results; the totals add up.
    pub fn merge(mut self, mut other: Self) -> Self {
        self.nodes.append(&mut other.nodes);
        self.links.append(&mut other.links);
        self.errors.append(&mut other.errors);
        self.direct_maxima.append(&mut other.direct_maxima);
        self.hidden.append(&mut other.hidden);
        self.cache_files.append(&mut other.cache_files);
        Self {
            files: self.files.merge(other.files),
            totals: self.totals.merge(other.totals),
            nodes: self.nodes,
            links: self.links,
            errors: self.errors,
            direct_maxima: self.direct_maxima,
            hidden: self.hidden,
            cache_files: self.cache_files,
        }
    }
}

/// The non-directory children of one directory.
#[derive(Debug)]
pub(super) struct Leaves {
    /// Their aggregate.
    pub totals: Totals,
    /// The biggest of them.
    pub files: TopFiles,
    /// Names of files that have more than one.
    pub links: Vec<LinkSighting>,
    /// How many entries were looked at.
    pub entries: u64,
    /// Every file above the cache's floor, for the scan cache.
    pub cache_files: Vec<FileEntry>,
}

impl Leaves {
    /// No leaves yet, keeping at most `cap` files.
    pub fn empty(cap: usize) -> Self {
        Self {
            totals: Totals::default(),
            files: TopFiles::new(cap),
            links: Vec::new(),
            entries: 0,
            cache_files: Vec::new(),
        }
    }

    /// Count one non-directory entry.
    ///
    /// A cloud placeholder counts as an entry and as nothing else: its bytes are
    /// not on this disk (`docs/cli-spec.md` §4.2). A name of a multiply-linked
    /// file is counted here and recorded, so the duplicates can be taken back
    /// out once the whole walk is known.
    pub fn add_leaf(mut self, path: &Path, meta: &EntryMetadata, context: &Context<'_>) -> Self {
        let entries = self.entries.saturating_add(1);
        if meta.is_dataless {
            return Self { entries, totals: self.totals.merge(Totals::leaf(meta)), ..self };
        }
        if meta.link_count > 1 {
            self.links.push(LinkSighting {
                device: meta.device,
                inode: meta.inode,
                path: path.to_path_buf(),
                link_count: meta.link_count,
                size_bytes: meta.size_bytes,
                allocated_bytes: meta.allocated_bytes,
            });
        }
        // Either size clears the threshold: the readers downstream filter on
        // the one they mean, and a sparse or compressed file is not lost here.
        let reportable = meta.size_bytes.max(meta.allocated_bytes);
        let reported = context.options.report_files_min_size.is_some_and(|min| reportable >= min);
        let cached = context.options.cache_files_min_size.is_some_and(|min| reportable >= min);
        if reported || cached {
            let entry = FileEntry {
                path: path.to_path_buf(),
                size_bytes: meta.size_bytes,
                allocated_bytes: meta.allocated_bytes,
                device: meta.device,
                inode: meta.inode,
                link_count: meta.link_count,
                modified: meta.modified,
                accessed: meta.accessed,
            };
            if cached {
                self.cache_files.push(entry.clone());
            }
            if reported {
                self.files = self.files.with(entry);
            }
        }
        Self { entries, totals: self.totals.merge(Totals::leaf(meta)), ..self }
    }
}

/// The children of one directory, split into what to descend into and what to count.
#[derive(Debug)]
pub(super) struct Children {
    /// Directories to walk, with the metadata already read.
    pub dirs: Vec<(PathBuf, EntryMetadata)>,
    /// Everything else, already counted.
    pub leaves: Leaves,
    /// Warnings about children that could not be read.
    pub errors: Vec<Diagnostic>,
    /// `true` when a child was excluded, unreadable, dataless, or elsewhere.
    pub skipped: bool,
}

impl Children {
    /// No children yet, keeping at most `cap` files.
    fn empty(cap: usize) -> Self {
        Self { dirs: Vec::new(), leaves: Leaves::empty(cap), errors: Vec::new(), skipped: false }
    }

    /// Classify one child and fold it in.
    fn add(mut self, child: PathBuf, meta: Result<EntryMetadata, BrozaError>, context: &Context<'_>) -> Self {
        if context.is_excluded(&child) {
            return Self { skipped: true, ..self };
        }
        let meta = match meta {
            Ok(meta) => meta,
            Err(error) => {
                self.errors.push(diagnostic(&child, &error));
                return Self { skipped: true, ..self };
            }
        };
        if context.crosses_device(&meta) {
            return Self { skipped: true, ..self };
        }
        let is_directory = meta.is_dir && !meta.is_symlink;
        // A dataless directory is a door to the provider's servers: opening it
        // blocks. It is counted and left closed.
        if is_directory && !meta.is_dataless {
            self.dirs.push((child, meta));
            return self;
        }
        let skipped = self.skipped || (is_directory && meta.is_dataless);
        let leaves = self.leaves.add_leaf(&child, &meta, context);
        Self { leaves, skipped, ..self }
    }
}

/// Split the children of a directory into subdirectories and counted leaves.
pub(super) fn split_children(
    listing: Vec<(PathBuf, Result<EntryMetadata, BrozaError>)>,
    context: &Context<'_>,
) -> Children {
    let cap = context.options.report_files_top;
    listing.into_iter().fold(Children::empty(cap), |acc, (child, meta)| acc.add(child, meta, context))
}

/// Turn a read failure into the warning the report carries.
///
/// A missing permission is the expected case on a Mac without Full Disk Access, so
/// it gets its own code and never aborts the walk (`docs/cli-spec.md` §7).
pub(super) fn diagnostic(path: &Path, error: &BrozaError) -> Diagnostic {
    let code = if matches!(error, BrozaError::PermissionDenied { .. }) {
        PERMISSION_DENIED_CODE
    } else {
        UNREADABLE_ENTRY_CODE
    };
    Diagnostic { code: code.to_owned(), message: error.to_string(), path: Some(path.to_path_buf()) }
}

/// Node of a directory whose children were not walked.
pub(super) fn truncated_node(identity: &DirIdentity, totals: Totals) -> DirNode {
    DirNode::new(identity, totals, true)
}
