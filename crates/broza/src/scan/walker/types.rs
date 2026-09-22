//! The vocabulary of a walk: what it is told, what it reports.
//!
//! Split out of [`super`] so both files stay small; everything here is
//! re-exported from the walker module, which is where callers import it from.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::model::Diagnostic;
use crate::ports::EntryMetadata;
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

/// A cache lookup: answers with the whole subtree as it was measured last
/// time, or nothing.
pub type SkipHook<'a> = &'a (dyn Fn(&DirIdentity) -> Option<CachedSubtree> + Sync);

/// A subtree the cache answered for: the directory asked about first, then
/// every directory below it, and every file the cache keeps (those at or above
/// the cache's own file floor). What a walk of the unchanged subtree would
/// have reported, without the walk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CachedSubtree {
    /// The directories, the one asked about first.
    pub nodes: Vec<DirNode>,
    /// The files the cache keeps, anywhere in the subtree.
    pub files: Vec<FileEntry>,
}

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
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent facts about one directory, not a state machine: whether its own \
              children were all reported, whether anything below was skipped, whether a \
              multiply-linked file lives inside, and whether the numbers were measured or recalled"
)]
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
    /// `true` when a file inside has a name *outside* this subtree.
    ///
    /// Not every hard link makes a subtree uncacheable — only one whose other
    /// names are elsewhere. A directory holding all forty names of one file is
    /// self-contained: a walk that skips it still counts that file once,
    /// because nobody else will count it. Settled after the walk, in
    /// `dedupe.rs`, when every name of every inode is known.
    pub has_hard_links: bool,
    /// `true` when something inside was not walked, at any depth.
    pub has_truncation: bool,
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
    /// `true` when these numbers came from the cache rather than from this
    /// walk. Such a node must not be recorded again: re-stamping it would
    /// renew a record that was never re-measured, and a subtree that keeps
    /// being served would then never expire.
    pub from_cache: bool,
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
            has_hard_links: false,
            has_truncation: totals.has_truncation || children_truncated,
            device: identity.device,
            inode: identity.inode,
            mtime: identity.mtime,
            children_truncated,
            from_cache: false,
        }
    }

    /// The same node, knowing one of its files has a name outside it.
    pub(super) fn hiding_a_name(self) -> Self {
        Self { has_hard_links: true, ..self }
    }

    /// Identity this node would be cached under.
    pub fn identity(&self) -> DirIdentity {
        DirIdentity { path: self.path.clone(), device: self.device, inode: self.inode, mtime: self.mtime }
    }
}

/// A file big enough to be worth reporting on its own, as the walk saw it.
///
/// Carries what `lstat` said at walk time so that detectors judge from one
/// consistent record instead of asking again: reading a file for comparison
/// updates its access time, and a second `stat` would see that, not the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Absolute path of the file.
    pub path: PathBuf,
    /// Apparent size in bytes.
    pub size_bytes: u64,
    /// Allocated size in bytes.
    pub allocated_bytes: u64,
    /// Device id (`st_dev`).
    pub device: u64,
    /// Inode number (`st_ino`).
    pub inode: u64,
    /// Number of hard links; more than one means other names share the bytes.
    pub link_count: u64,
    /// Last modification time.
    pub modified: Option<Timestamp>,
    /// Last access time, before any detector read the file.
    pub accessed: Option<Timestamp>,
}

impl FileEntry {
    /// A file of `size_bytes` with one name and no known dates, for tests.
    #[cfg(test)]
    pub(crate) fn sized(path: &str, size_bytes: u64) -> Self {
        Self {
            path: PathBuf::from(path),
            size_bytes,
            allocated_bytes: size_bytes,
            device: 0,
            inode: 0,
            link_count: 1,
            modified: None,
            accessed: None,
        }
    }
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
    /// Collect every file at least this big into [`WalkResult::cache_files`],
    /// unbounded, for the scan cache to keep; `None` collects none.
    pub cache_files_min_size: Option<u64>,
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
            cache_files_min_size: None,
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
            .field("cache_files_min_size", &self.cache_files_min_size)
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
    /// `true` when more files passed the threshold than `report_files_top`
    /// allowed to keep, so `files` is the biggest of them, not all of them.
    pub files_truncated: bool,
    /// Every file above the cache's floor, for the scan cache, sorted by path.
    pub cache_files: Vec<FileEntry>,
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
