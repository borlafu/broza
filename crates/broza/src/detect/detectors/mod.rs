//! The detectors Broza ships, one file per category.

pub mod build_cache;
pub mod cloud_synced;
pub mod duplicates;
pub mod ios_simulators;
pub mod large_old_files;
mod node_modules;
pub mod old_backups;
pub mod snapshots;
pub mod trash;
pub mod unused_apps;
pub mod user_cache;

use super::detector::Detector;

/// Every built-in detector, in category order.
pub fn builtin() -> Vec<Box<dyn Detector>> {
    vec![
        Box::new(user_cache::UserCache),
        Box::new(build_cache::BuildCache),
        Box::new(trash::Trash),
        Box::new(snapshots::Snapshots),
        Box::new(old_backups::OldBackups),
        Box::new(ios_simulators::IosSimulators),
        Box::new(large_old_files::LargeOldFiles),
        Box::new(duplicates::Duplicates),
        Box::new(unused_apps::UnusedApps),
        Box::new(cloud_synced::CloudSynced),
    ]
}

/// Shared helpers for detectors.
pub(super) mod support {
    use std::cmp::Ordering;
    use std::path::Path;

    use jiff::Timestamp;

    use crate::BrozaError;
    use crate::model::{Category, Finding, FindingBuilder, FindingId, FindingPath};
    use crate::scan::DirNode;

    /// A finding path measured by the walk: allocated bytes, and the directory's
    /// mtime as `last_used`.
    ///
    /// A directory's mtime is when an entry was last added or removed inside it,
    /// not when its contents were last read: an approximation of "last used",
    /// good enough to sort caches, and what `--explain` shows. `atime` and
    /// Spotlight's last-used date (`docs/cli-spec.md` §3.3) are for the
    /// detectors that judge single files.
    pub fn path_of(node: &DirNode) -> FindingPath {
        FindingPath { path: node.path.clone(), size_bytes: node.allocated_bytes, last_used: node.mtime }
    }

    /// A finding path for something the walk did not measure.
    pub fn path_with(path: &Path, size_bytes: u64, last_used: Option<Timestamp>) -> FindingPath {
        FindingPath { path: path.to_path_buf(), size_bytes, last_used }
    }

    /// The order every finding lists its paths in: biggest first, then by path.
    pub fn by_size_then_path(a: &FindingPath, b: &FindingPath) -> Ordering {
        b.size_bytes.cmp(&a.size_bytes).then_with(|| a.path.cmp(&b.path))
    }

    /// Start a finding of `category` with id `<category>.<detector>`.
    ///
    /// # Errors
    ///
    /// Only when `detector` is not kebab-case, which is a programming error.
    pub fn start(category: Category, detector: &str, title: &str) -> Result<FindingBuilder, BrozaError> {
        let id: FindingId = format!("{}.{detector}", category.as_str()).parse()?;
        Ok(Finding::builder(id, category, title))
    }

    /// Finish a finding over `paths`, or nothing when there is nothing to report.
    ///
    /// `reclaimable_bytes` and `item_count` are derived from the paths so the
    /// numbers a finding prints are the numbers its paths add up to.
    ///
    /// # Errors
    ///
    /// When the finding would break an invariant of the model.
    pub fn finish(builder: FindingBuilder, paths: Vec<FindingPath>) -> Result<Option<Finding>, BrozaError> {
        if paths.is_empty() {
            return Ok(None);
        }
        let bytes = paths.iter().fold(0_u64, |sum, path| sum.saturating_add(path.size_bytes));
        let count = u64::try_from(paths.len()).unwrap_or(u64::MAX);
        builder.reclaimable_bytes(bytes).item_count(count).paths(paths).build().map(Some)
    }
}
