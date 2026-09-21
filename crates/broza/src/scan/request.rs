//! What a scan was asked for, and what one volume's scan produced.

use std::path::PathBuf;
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
    use std::path::PathBuf;

    use super::{DEFAULT_DEPTH, DEFAULT_MIN_SIZE_BYTES, DEFAULT_TOP, ScanRequest};

    #[test]
    fn the_defaults_are_the_ones_the_specification_documents() {
        let request = ScanRequest::default();

        assert_eq!(request.depth, DEFAULT_DEPTH);
        assert_eq!(request.top, DEFAULT_TOP);
        assert_eq!(request.min_size, DEFAULT_MIN_SIZE_BYTES);
        assert_eq!((DEFAULT_DEPTH, DEFAULT_TOP, DEFAULT_MIN_SIZE_BYTES), (2, 20, 100_000_000));
        assert!(request.include_external, "externals are included unless --no-external");
        assert!(!request.no_cache);
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
