//! The leaves and the children of one directory: what the [walker](super)
//! counts at each level before descending.
//!
//! Split out of `parts.rs` so that both files stay small; nothing here is part
//! of the public surface.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::BrozaError;
use crate::model::Diagnostic;
use crate::ports::EntryMetadata;
use crate::scan::walker::FileEntry;
use crate::scan::walker::clones::{CloneLedger, Originals};
use crate::scan::walker::parts::{Context, LinkSighting, Totals, diagnostic, is_shared};
use crate::scan::walker::top_files::TopFiles;

/// The non-directory children of one directory.
#[derive(Debug)]
pub(super) struct Leaves {
    /// Their aggregate.
    pub totals: Totals,
    /// The biggest of them.
    pub files: TopFiles,
    /// The biggest of those whose bytes other names may share.
    pub shared_files: TopFiles,
    /// Names of files that have more than one.
    pub links: Vec<LinkSighting>,
    /// How many entries were looked at.
    pub entries: u64,
    /// Every file above the cache's floor, for the scan cache.
    pub cache_files: Vec<FileEntry>,
    /// Every file that could be the original of a clone family.
    pub originals: Originals,
    /// Every clone met, by family and by directory.
    pub clones: CloneLedger,
}

impl Leaves {
    /// No leaves yet, keeping at most `cap` files.
    pub fn empty(cap: usize) -> Self {
        Self {
            totals: Totals::default(),
            files: TopFiles::new(cap),
            shared_files: TopFiles::new(cap),
            links: Vec::new(),
            entries: 0,
            cache_files: Vec::new(),
            originals: Originals::default(),
            clones: CloneLedger::default(),
        }
    }

    /// Count one non-directory entry, directly inside `dir`.
    ///
    /// A cloud placeholder counts as an entry and as nothing else: its bytes are
    /// not on this disk (`docs/cli-spec.md` §4.2). A name of a multiply-linked
    /// file is counted here and recorded, and so is a clone, in the ledger, so
    /// the shared bytes can be taken back out once the whole walk is known; a
    /// file that could be the original of a clone family is remembered by
    /// inode for the same reason.
    pub fn add_leaf(
        mut self,
        dir: &Arc<Path>,
        path: PathBuf,
        meta: &EntryMetadata,
        context: &Context<'_>,
    ) -> Self {
        let entries = self.entries.saturating_add(1);
        if meta.is_dataless {
            return Self { entries, totals: self.totals.merge(Totals::leaf(meta)), ..self };
        }
        if meta.clone_id == Some(meta.inode) {
            self.originals.record(meta.device, meta.inode);
        }
        // Either size clears the threshold: the readers downstream filter on
        // the one they mean, and a sparse or compressed file is not lost here.
        let reportable = meta.size_bytes.max(meta.allocated_bytes);
        let reported = context.options.report_files_min_size.is_some_and(|min| reportable >= min);
        let cached = context.options.cache_files_min_size.is_some_and(|min| reportable >= min);
        let shared = is_shared(meta);
        let (for_entry, for_link) = if shared { (path.clone(), Some(path)) } else { (path, None) };
        if reported || cached {
            let entry = FileEntry {
                path: for_entry,
                size_bytes: meta.size_bytes,
                allocated_bytes: meta.allocated_bytes,
                device: meta.device,
                inode: meta.inode,
                link_count: meta.link_count,
                modified: meta.modified,
                accessed: meta.accessed,
                clone_id: meta.clone_id,
            };
            if cached {
                self.cache_files.push(entry.clone());
            }
            if reported && shared {
                self.shared_files = self.shared_files.with(entry);
            } else if reported {
                self.files = self.files.with(entry);
            }
        }
        // A clone that also has several names is a hard link first: the names
        // settle to one, and that one is counted where it stands.
        match for_link {
            Some(path) if meta.link_count > 1 => self.links.push(LinkSighting {
                device: meta.device,
                inode: meta.inode,
                path,
                link_count: meta.link_count,
                size_bytes: meta.size_bytes,
                allocated_bytes: meta.allocated_bytes,
            }),
            Some(path) => self.clones.record(dir, path, meta),
            None => {}
        }
        Self { entries, totals: self.totals.merge(Totals::leaf(meta)), ..self }
    }
}

/// The children of one directory, split into what to descend into and what to count.
#[derive(Debug)]
pub(super) struct Children {
    /// The directory itself, shared with every record that names it.
    dir: Arc<Path>,
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
    /// No children of `dir` yet, keeping at most `cap` files.
    fn empty(dir: Arc<Path>, cap: usize) -> Self {
        Self { dir, dirs: Vec::new(), leaves: Leaves::empty(cap), errors: Vec::new(), skipped: false }
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
        let leaves = self.leaves.add_leaf(&self.dir, child, &meta, context);
        Self { leaves, skipped, ..self }
    }
}

/// Split the children of a directory into subdirectories and counted leaves.
pub(super) fn split_children(
    dir: &Path,
    listing: Vec<(PathBuf, Result<EntryMetadata, BrozaError>)>,
    context: &Context<'_>,
) -> Children {
    let cap = context.options.report_files_top;
    let start = Children::empty(Arc::from(dir), cap);
    listing.into_iter().fold(start, |acc, (child, meta)| acc.add(child, meta, context))
}
