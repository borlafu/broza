//! The vocabulary of a walk: what it is told, what it reports.
//!
//! Split out of [`super`] so both files stay small; everything here is
//! re-exported from the walker module, which is where callers import it from.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::model::Diagnostic;
use crate::ports::EntryMetadata;
use crate::scan::cache::DirRecord;
use crate::scan::progress::ProgressReporter;
use crate::scan::walker::parts::Totals;

/// Warning code for a path Broza is not allowed to read.
pub const PERMISSION_DENIED_CODE: &str = "permission_denied";
/// Warning code for a path that could not be read for any other reason.
pub const UNREADABLE_ENTRY_CODE: &str = "unreadable_entry";
/// Shallowest depth a cached answer may be used at, counting the root as 0.
///
/// The root is always walked: a scan whose first question is "has anything
/// changed here?" would answer "no" and report nothing at all.
pub const MIN_CACHE_DEPTH: usize = 1;

/// A cache lookup: answers with what was measured last time, or nothing.
pub type SkipHook<'a> = &'a (dyn Fn(&DirIdentity) -> Option<DirRecord> + Sync);

/// What the scan cache keys a directory by (`docs/implementation-plan.md` §3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirIdentity {
    /// Absolute path of the directory.
    pub path: PathBuf,
    /// Device id (`st_dev`).
    pub device: u64,
    /// Inode number (`st_ino`).
    pub inode: u64,
    /// Last modification time; `None` when the filesystem reported one out of range.
    pub mtime: Option<Timestamp>,
}

impl DirIdentity {
    /// Identity of the directory at `path`.
    pub(super) fn of(path: &Path, meta: &EntryMetadata) -> Self {
        Self { path: path.to_path_buf(), device: meta.device, inode: meta.inode, mtime: meta.modified }
    }
}

/// One directory and the aggregate of everything below it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirNode {
    /// Absolute path of the directory.
    pub path: PathBuf,
    /// Apparent bytes of every file and symlink in the subtree.
    pub size_bytes: u64,
    /// Allocated bytes of the same entries.
    pub allocated_bytes: u64,
    /// Non-directory entries in the subtree, cloud placeholders included.
    pub file_count: u64,
    /// Directories in the subtree, excluding this one.
    pub dir_count: u64,
    /// Cloud placeholders in the subtree, whose bytes are not on this disk.
    pub dataless_count: u64,
    /// Biggest single reportable thing inside: the largest file, or the largest
    /// descendant directory's subtree.
    pub largest_item_bytes: u64,
    /// Device id (`st_dev`).
    pub device: u64,
    /// Inode number (`st_ino`).
    pub inode: u64,
    /// Last modification time.
    pub mtime: Option<Timestamp>,
    /// `true` when some child was not reported: depth limit, exclusion, another
    /// device, a cloud placeholder, or an unreadable entry. A subtree served
    /// from the cache is **not** truncated — it was measured, just not again.
    pub children_truncated: bool,
}

impl DirNode {
    /// Node of the directory `identity` describes.
    pub(super) fn new(identity: &DirIdentity, totals: Totals, children_truncated: bool) -> Self {
        Self {
            path: identity.path.clone(),
            size_bytes: totals.size_bytes,
            allocated_bytes: totals.allocated_bytes,
            file_count: totals.file_count,
            dir_count: totals.dir_count,
            dataless_count: totals.dataless_count,
            largest_item_bytes: totals.largest_item_bytes,
            device: identity.device,
            inode: identity.inode,
            mtime: identity.mtime,
            children_truncated,
        }
    }

    /// Identity this node would be cached under.
    pub fn identity(&self) -> DirIdentity {
        DirIdentity { path: self.path.clone(), device: self.device, inode: self.inode, mtime: self.mtime }
    }
}

/// A file big enough to be worth reporting on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Absolute path of the file.
    pub path: PathBuf,
    /// Apparent size in bytes.
    pub size_bytes: u64,
    /// Allocated size in bytes.
    pub allocated_bytes: u64,
}

/// How a walk behaves.
///
/// Symlinks are deliberately not an option: they are never followed.
pub struct WalkOptions<'a> {
    /// Deepest level still reported, counting the root as 0. `None` reports all.
    ///
    /// Reporting only: the totals always cover the whole subtree.
    pub max_depth: Option<usize>,
    /// Never descend into a directory on another device. On by default, because
    /// another device is another volume, scanned on its own.
    pub same_device_only: bool,
    /// Path prefixes that are not visited at all.
    pub exclude: Vec<PathBuf>,
    /// Cache lookup: when it answers, the subtree is not walked again.
    pub skip_hook: Option<SkipHook<'a>>,
    /// Shallowest depth the cache may answer at; never below [`MIN_CACHE_DEPTH`].
    pub cache_from_depth: usize,
    /// Collect files at least this big into [`WalkResult::files`]; `None`
    /// collects none.
    pub report_files_min_size: Option<u64>,
    /// How many files to keep at most, so a walk of a million of them stays
    /// bounded in memory.
    pub report_files_top: usize,
    /// Where progress updates go.
    pub progress: Option<&'a ProgressReporter<'a>>,
}

impl Default for WalkOptions<'_> {
    fn default() -> Self {
        Self {
            max_depth: None,
            same_device_only: true,
            exclude: Vec::new(),
            skip_hook: None,
            cache_from_depth: MIN_CACHE_DEPTH,
            report_files_min_size: None,
            report_files_top: 0,
            progress: None,
        }
    }
}

impl WalkOptions<'_> {
    /// `true` when the cache may answer for a directory at `depth`.
    pub(super) fn cache_answers_at(&self, depth: usize) -> bool {
        depth >= self.cache_from_depth.max(MIN_CACHE_DEPTH)
    }
}

impl std::fmt::Debug for WalkOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalkOptions")
            .field("max_depth", &self.max_depth)
            .field("same_device_only", &self.same_device_only)
            .field("exclude", &self.exclude)
            .field("cache_from_depth", &self.cache_from_depth)
            .field("report_files_min_size", &self.report_files_min_size)
            .field("report_files_top", &self.report_files_top)
            .finish_non_exhaustive()
    }
}

/// Everything one walk found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkResult {
    /// One node per reported directory, sorted by path.
    pub nodes: Vec<DirNode>,
    /// The biggest files above the reporting threshold, sorted by path.
    pub files: Vec<FileEntry>,
    /// Warnings about what could not be read, sorted by path.
    pub errors: Vec<Diagnostic>,
}

impl WalkResult {
    /// The node of the directory the walk started from.
    pub fn root(&self) -> Option<&DirNode> {
        self.nodes.first()
    }

    /// Paths of every reported node, in report order.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.nodes.iter().map(|node| node.path.clone()).collect()
    }
}
