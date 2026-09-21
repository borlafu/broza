//! What a scan was asked for, and what one volume's scan produced.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::model::{Diagnostic, LargestItem, VolumeId};
use crate::scan::aggregate::TreeView;
use crate::scan::walker::DirNode;

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
pub const CLOUD_ROOTS: [&str; 2] = ["Library/Mobile Documents", "Library/CloudStorage"];

/// The prefixes a scan of `home` stays out of by default.
///
/// Naming one of them explicitly still scans it: this is a default, not a refusal.
pub fn default_excludes(home: &Path) -> Vec<PathBuf> {
    CLOUD_ROOTS.iter().map(|relative| home.join(relative)).collect()
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
    /// `--no-cache`: ignore what is cached. A fresh cache is still written.
    pub no_cache: bool,
    /// Where the cache lives (`~/.cache/broza`); `None` disables it entirely.
    pub cache_root: Option<PathBuf>,
    /// How long a cached record stays usable.
    pub cache_ttl: Duration,
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
            no_cache: false,
            cache_root: None,
            cache_ttl: DEFAULT_CACHE_TTL,
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
}

/// What scanning one volume produced.
#[derive(Debug, Clone, PartialEq)]
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
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        CLOUD_ROOTS, DEFAULT_DEPTH, DEFAULT_MIN_SIZE_BYTES, DEFAULT_TOP, ScanRequest, default_excludes,
    };

    #[test]
    fn the_defaults_are_the_ones_the_specification_documents() {
        let request = ScanRequest::default();

        assert_eq!(request.depth, DEFAULT_DEPTH);
        assert_eq!(request.top, DEFAULT_TOP);
        assert_eq!(request.min_size, DEFAULT_MIN_SIZE_BYTES);
        assert!(request.include_external, "externals are included unless --no-external");
        assert!(!request.no_cache);
    }

    #[test]
    fn a_request_for_a_home_stays_out_of_the_cloud_providers_roots() {
        let request = ScanRequest::for_home(Path::new("/Users/dana"));

        assert_eq!(
            request.exclude,
            vec![
                PathBuf::from("/Users/dana/Library/Mobile Documents"),
                PathBuf::from("/Users/dana/Library/CloudStorage"),
            ]
        );
        assert_eq!(request.depth, ScanRequest::default().depth, "nothing else changes");
    }

    #[test]
    fn the_excluded_roots_are_named_relative_to_the_home_they_are_under() {
        let mine = default_excludes(Path::new("/Users/dana"));
        let yours = default_excludes(Path::new("/Users/sam"));

        assert!(mine.iter().all(|path| path.starts_with("/Users/dana")));
        assert!(yours.iter().all(|path| path.starts_with("/Users/sam")));
        assert_eq!(mine.len(), CLOUD_ROOTS.len());
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
