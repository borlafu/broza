//! What a scan was asked for, and what one volume's scan produced.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::model::{Diagnostic, LargestItem, VolumeId};
use crate::safety::firmlink::firmlink_spellings;
use crate::scan::aggregate::TreeView;
use crate::scan::walker::{DirNode, FileEntry};

/// Default `--depth` (`docs/cli-spec.md` §3.1).
pub const DEFAULT_DEPTH: usize = 2;
/// Default `--top`.
pub const DEFAULT_TOP: usize = 20;
/// Default `--min-size`, 100 MB.
pub const DEFAULT_MIN_SIZE_BYTES: u64 = 100_000_000;
/// Default `cache-ttl`, 24 hours.
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Directories, relative to the home directory, that a scan stays out of.
///
/// These are the cloud providers' roots. Their files are placeholders until
/// somebody opens them, and `lstat` on one blocks on the provider: a scan that
/// walks in measures the network instead of the disk, for minutes at a time
/// (`docs/cli-spec.md` §7). Broza reports what is really on the disk and leaves
/// the provider's copy to the provider, which is also what invariant §2.5 asks.
pub const CLOUD_ROOTS: [&str; 5] =
    ["Library/Mobile Documents", "Library/CloudStorage", "Dropbox", "OneDrive", "Google Drive"];
/// Smallest file the home walk of `suggest` reports one by one: the
/// `duplicates` threshold, 1 MB counted the way Finder counts. The
/// `large-old-files` detector keeps only the 1 GB ones of those.
pub const DETECTOR_FILES_MIN_BYTES: u64 = 1_000_000;
/// Most files that walk keeps, the biggest first: a bound on memory, not a
/// promise to see every last one on a home with more.
pub const DETECTOR_FILES_TOP: usize = 200_000;

/// Which files a walk reports one by one ([`crate::scan::WalkResult::files`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileReport {
    /// Files below this many bytes are not reported.
    pub min_size: u64,
    /// At most this many files, the biggest.
    pub top: usize,
}

impl FileReport {
    /// The report the detectors of `suggest` read (`docs/cli-spec.md` §3.3).
    #[must_use]
    pub fn for_detectors() -> Self {
        Self { min_size: DETECTOR_FILES_MIN_BYTES, top: DETECTOR_FILES_TOP }
    }
}

/// The prefixes a scan of `home` stays out of by default.
///
/// Naming one of them explicitly still scans it: this is a default, not a refusal.
pub fn default_excludes(home: &Path) -> Vec<PathBuf> {
    // A whole-volume walk roots at `/System/Volumes/Data`, so the same folder
    // is met in that spelling: exclude both, or the exclusion never fires.
    CLOUD_ROOTS
        .iter()
        .flat_map(|relative| firmlink_spellings(&home.join(relative)))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// How a scan uses the per-volume store (`docs/cli-spec.md` §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheUse {
    /// Load the store and serve unchanged subtrees from it; refresh it after.
    #[default]
    Serve,
    /// Load the store but walk everything; refresh it after. What `clean`
    /// does: no cached size reaches the guard, and the records for the rest
    /// of the volume survive. A store that cannot be read is replaced.
    Refresh,
    /// `--no-cache`: do not read the store at all; a fresh one is written.
    Bypass,
}

/// A `scan` as the caller asked for it.
///
/// The core never reads the environment, so the cache location arrives here
/// rather than being derived from `$HOME` (`AGENTS.md` §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRequest {
    /// `--volume`: a volume id, name, or mount point. `None` means every volume.
    pub volume: Option<String>,
    /// Whether volumes mounted under `/Volumes` are included (`--no-external`).
    pub include_external: bool,
    /// `--depth`: how deep the tree view goes.
    pub depth: usize,
    /// `--top`: how many largest items to report.
    pub top: usize,
    /// `--min-size`: items below this are ignored.
    pub min_size: u64,
    /// Path prefixes the walk never enters.
    pub exclude: Vec<PathBuf>,
    /// How the per-volume store is used.
    pub cache: CacheUse,
    /// Where the cache lives (`~/.cache/broza`); `None` disables it entirely.
    pub cache_root: Option<PathBuf>,
    /// How long a cached record stays usable.
    pub cache_ttl: Duration,
    /// Keep every `permission_denied` warning instead of one counted summary (`-v`).
    pub verbose_warnings: bool,
    /// Which files to report one by one. `None` reports the `--top` largest
    /// above `--min-size`, which is what `scan` lists.
    pub file_report: Option<FileReport>,
}

impl Default for ScanRequest {
    fn default() -> Self {
        Self {
            volume: None,
            include_external: true,
            depth: DEFAULT_DEPTH,
            top: DEFAULT_TOP,
            min_size: DEFAULT_MIN_SIZE_BYTES,
            exclude: Vec::new(),
            cache: CacheUse::Serve,
            cache_root: None,
            cache_ttl: DEFAULT_CACHE_TTL,
            verbose_warnings: false,
            file_report: None,
        }
    }
}

impl ScanRequest {
    /// A request for a user's own machine: the cloud roots under `home` are
    /// excluded, for the reason [`CLOUD_ROOTS`] explains.
    #[must_use]
    pub fn for_home(home: &Path) -> Self {
        Self { exclude: default_excludes(home), ..Self::default() }
    }

    /// The same request, aimed at one volume.
    #[must_use]
    pub fn for_volume(&self, volume: impl Into<String>) -> Self {
        Self { volume: Some(volume.into()), ..self.clone() }
    }

    /// The files this request wants one by one: its own report, or the
    /// `--top` largest above `--min-size`.
    #[must_use]
    pub fn file_report(&self) -> FileReport {
        self.file_report.unwrap_or(FileReport { min_size: self.min_size, top: self.top })
    }

    /// The smallest thing this request lists on its own. A cached subtree that
    /// hides something this big is walked again, so a warm scan lists what a
    /// cold one does.
    #[must_use]
    pub fn reporting_floor(&self) -> u64 {
        self.min_size.min(self.file_report().min_size)
    }
}

/// What scanning one volume produced.
///
/// Equality compares what is *reported* — root, largest items, tree, warnings —
/// and deliberately not `nodes`: a warm scan reports exactly what a cold one
/// does while its nodes carry `from_cache` marks the cold ones do not.
#[derive(Debug, Clone)]
pub struct VolumeScan {
    /// Volume the numbers belong to.
    pub volume_id: VolumeId,
    /// Aggregate of the whole volume, rooted at its mount point.
    pub root: DirNode,
    /// Largest items, biggest first (`docs/cli-spec.md` §4.2).
    pub largest: Vec<LargestItem>,
    /// Nested view for the human renderer.
    pub tree: TreeView,
    /// What could not be read. Warnings never change the exit code.
    pub warnings: Vec<Diagnostic>,
    /// Every directory the walk measured, for detectors that read the tree.
    pub nodes: Vec<DirNode>,
    /// The files the request asked to see one by one, sorted by path.
    pub files: Vec<FileEntry>,
    /// Every APFS clone family the walk knows of, as `(device, original inode)`, sorted.
    pub clone_families: Vec<(u64, u64)>,
    /// `true` when macOS refused to list the scan's own root for lack of
    /// permission: the whole scan was impossible, not merely incomplete.
    pub root_refused: bool,
}

impl PartialEq for VolumeScan {
    fn eq(&self, other: &Self) -> bool {
        self.volume_id == other.volume_id
            && self.root == other.root
            && self.largest == other.largest
            && self.tree == other.tree
            && self.warnings == other.warnings
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        CLOUD_ROOTS, CacheUse, DEFAULT_DEPTH, DEFAULT_MIN_SIZE_BYTES, DEFAULT_TOP, ScanRequest,
        default_excludes,
    };

    #[test]
    fn the_defaults_are_the_ones_the_specification_documents() {
        let request = ScanRequest::default();

        assert_eq!(request.depth, DEFAULT_DEPTH);
        assert_eq!(request.top, DEFAULT_TOP);
        assert_eq!(request.min_size, DEFAULT_MIN_SIZE_BYTES);
        assert!(request.include_external, "externals are included unless --no-external");
        assert_eq!(request.cache, CacheUse::Serve);
    }

    #[test]
    fn a_request_for_a_home_stays_out_of_the_cloud_providers_roots() {
        let request = ScanRequest::for_home(Path::new("/Users/dana"));

        // Both firmlink spellings: a whole-volume walk meets the folder as
        // `/System/Volumes/Data/Users/...`, an explicit path as `/Users/...`.
        for spelling in [
            "/Users/dana/Library/Mobile Documents",
            "/Users/dana/Library/CloudStorage",
            "/System/Volumes/Data/Users/dana/Library/Mobile Documents",
            "/System/Volumes/Data/Users/dana/Library/CloudStorage",
        ] {
            assert!(request.exclude.contains(&PathBuf::from(spelling)), "{spelling}: {:?}", request.exclude);
        }
        assert_eq!(request.depth, ScanRequest::default().depth, "nothing else changes");
    }

    #[test]
    fn the_excluded_roots_are_named_relative_to_the_home_they_are_under() {
        let mine = default_excludes(Path::new("/Users/dana"));
        let yours = default_excludes(Path::new("/Users/sam"));

        assert!(mine.iter().all(|path| path.to_string_lossy().contains("/Users/dana/")));
        assert!(yours.iter().all(|path| path.to_string_lossy().contains("/Users/sam/")));
        assert_eq!(mine.len(), CLOUD_ROOTS.len() * 2, "two spellings each");
    }

    #[test]
    fn aiming_a_request_at_a_volume_leaves_the_original_alone() {
        let original = ScanRequest { exclude: vec![PathBuf::from("/x")], ..ScanRequest::default() };

        let aimed = original.for_volume("disk3s5");

        assert_eq!(aimed.volume.as_deref(), Some("disk3s5"));
        assert_eq!(aimed.exclude, original.exclude);
        assert_eq!(original.volume, None);
    }
}
