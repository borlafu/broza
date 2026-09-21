//! Internals of the [walker](super): shared context, running totals, child triage.
//!
//! Split out of `walker.rs` so that both files stay small; nothing here is part of
//! the public surface.

use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::model::Diagnostic;
use crate::ports::{EntryMetadata, FileOps};
use crate::scan::links::LinkRegistry;
use crate::scan::walker::{
    DirIdentity, DirNode, FileEntry, PERMISSION_DENIED_CODE, UNREADABLE_ENTRY_CODE, WalkOptions,
};

/// Everything one walk shares across its threads.
pub(super) struct Context<'a> {
    /// Filesystem the walk reads through.
    pub fs: &'a dyn FileOps,
    /// Options the caller asked for.
    pub options: &'a WalkOptions<'a>,
    /// Inodes already counted, so a hard link is counted once.
    pub links: LinkRegistry,
    /// Device of the root; the walk never leaves it unless asked to.
    pub root_device: u64,
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
        self.options.exclude.iter().any(|prefix| path.starts_with(prefix))
    }

    /// `true` when following this entry would leave the root's device.
    pub fn crosses_device(&self, meta: &EntryMetadata) -> bool {
        self.options.same_device_only && meta.device != self.root_device
    }

    /// `true` when this name is the first one seen for its inode.
    ///
    /// Entries with a single link never touch the registry, which keeps the shared
    /// state limited to the few files that really have several names.
    pub fn counts_bytes(&self, meta: &EntryMetadata) -> bool {
        meta.link_count <= 1 || self.links.claim(meta.device, meta.inode)
    }
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
}

impl Totals {
    /// Totals of a single non-directory entry.
    pub fn leaf(meta: &EntryMetadata) -> Self {
        Self {
            size_bytes: meta.size_bytes,
            allocated_bytes: meta.allocated_bytes,
            file_count: 1,
            dir_count: 0,
        }
    }

    /// Sum of two subtrees, saturating instead of wrapping.
    pub fn merge(self, other: Self) -> Self {
        Self {
            size_bytes: self.size_bytes.saturating_add(other.size_bytes),
            allocated_bytes: self.allocated_bytes.saturating_add(other.allocated_bytes),
            file_count: self.file_count.saturating_add(other.file_count),
            dir_count: self.dir_count.saturating_add(other.dir_count),
        }
    }

    /// The same totals, plus the directories that are direct children.
    pub fn with_child_dirs(self, count: u64) -> Self {
        Self { dir_count: self.dir_count.saturating_add(count), ..self }
    }
}

/// What one recursion step contributes to the walk.
#[derive(Debug, Default)]
pub(super) struct Partial {
    /// Directory nodes worth reporting.
    pub nodes: Vec<DirNode>,
    /// Files above the reporting threshold.
    pub files: Vec<FileEntry>,
    /// Warnings collected on the way.
    pub errors: Vec<Diagnostic>,
    /// Aggregate of the subtree.
    pub totals: Totals,
}

impl Partial {
    /// Concatenate two partial results; the totals add up.
    pub fn merge(mut self, mut other: Self) -> Self {
        self.nodes.append(&mut other.nodes);
        self.files.append(&mut other.files);
        self.errors.append(&mut other.errors);
        Self { totals: self.totals.merge(other.totals), ..self }
    }
}

/// The non-directory children of one directory.
#[derive(Debug, Default)]
pub(super) struct Leaves {
    /// Their aggregate.
    pub totals: Totals,
    /// The ones above the reporting threshold.
    pub files: Vec<FileEntry>,
    /// How many entries were looked at, deduplicated links included.
    pub entries: u64,
}

impl Leaves {
    /// Count one non-directory entry.
    ///
    /// A second name for an already counted inode still counts as an entry seen,
    /// but contributes neither bytes nor a file to the report.
    pub fn add_leaf(mut self, path: &Path, meta: &EntryMetadata, context: &Context<'_>) -> Self {
        let entries = self.entries.saturating_add(1);
        if !context.counts_bytes(meta) {
            return Self { entries, ..self };
        }
        if context.options.report_files_min_size.is_some_and(|min| meta.size_bytes >= min) {
            self.files.push(FileEntry {
                path: path.to_path_buf(),
                size_bytes: meta.size_bytes,
                allocated_bytes: meta.allocated_bytes,
            });
        }
        Self { entries, totals: self.totals.merge(Totals::leaf(meta)), ..self }
    }
}

/// The children of one directory, split into what to descend into and what to count.
#[derive(Debug, Default)]
pub(super) struct Children {
    /// Directories to walk, with the metadata already read.
    pub dirs: Vec<(PathBuf, EntryMetadata)>,
    /// Everything else, already counted.
    pub leaves: Leaves,
    /// Warnings about children that could not be read.
    pub errors: Vec<Diagnostic>,
    /// `true` when a child was excluded, unreadable, or on another device.
    pub skipped: bool,
}

impl Children {
    /// Classify one child and fold it in.
    fn add(mut self, child: PathBuf, context: &Context<'_>) -> Self {
        if context.is_excluded(&child) {
            return Self { skipped: true, ..self };
        }
        let meta = match context.fs.metadata(&child) {
            Ok(meta) => meta,
            Err(error) => {
                self.errors.push(diagnostic(&child, &error));
                return Self { skipped: true, ..self };
            }
        };
        if context.crosses_device(&meta) {
            return Self { skipped: true, ..self };
        }
        if meta.is_dir && !meta.is_symlink {
            self.dirs.push((child, meta));
            return self;
        }
        let leaves = self.leaves.add_leaf(&child, &meta, context);
        Self { leaves, ..self }
    }
}

/// Split the children of a directory into subdirectories and counted leaves.
pub(super) fn split_children(children: Vec<PathBuf>, context: &Context<'_>) -> Children {
    children.into_iter().fold(Children::default(), |acc, child| acc.add(child, context))
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
