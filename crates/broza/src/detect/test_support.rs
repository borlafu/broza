//! A detection world over a fake filesystem, for detector tests.
//!
//! Walks the fake home with the real scanner so detectors see exactly the nodes
//! `suggest` would hand them, then lends out a [`DetectContext`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;

use crate::detect::detector::DetectContext;
use crate::scan::{DirNode, MountTable, WalkOptions, default_excludes, walk};
use crate::testing::{FakeFileOps, FakeSnapshots, mac_mount_table};

/// The home every detector test uses, in the Data-volume spelling the walk sees.
pub fn home() -> PathBuf {
    PathBuf::from("/System/Volumes/Data/Users/dana")
}

/// Everything a [`DetectContext`] borrows, owned in one place.
pub struct World<'a> {
    fs: &'a FakeFileOps,
    home: PathBuf,
    mounts: MountTable,
    nodes: Vec<DirNode>,
    snapshots: FakeSnapshots,
}

impl World<'_> {
    /// The context detectors are handed. `now` is fixed so "unused" is decidable.
    pub fn context(&self) -> DetectContext<'_> {
        DetectContext::new(
            &self.home,
            self.fs,
            &self.mounts,
            "2026-09-21T10:00:00Z".parse::<Timestamp>().unwrap_or_default(),
            Duration::from_secs(365 * 24 * 60 * 60),
            &self.nodes,
            &self.snapshots,
        )
    }

    /// The same world with `snapshots` reported for `volume`.
    #[must_use]
    pub fn with_snapshots(
        self,
        volume: crate::model::VolumeId,
        snapshots: Vec<crate::model::Snapshot>,
    ) -> Self {
        Self { snapshots: self.snapshots.with_snapshots(volume, snapshots), ..self }
    }
}

/// Walk `home` on `fs` the way `suggest` does — cloud roots and Broza's own
/// stores excluded, no file list — and package the result.
pub fn context_over(fs: &FakeFileOps, home: PathBuf) -> World<'_> {
    let mut exclude = default_excludes(&home);
    exclude.push(home.join(".cache/broza"));
    exclude.push(home.join(".local/share/broza"));
    let options = WalkOptions { exclude, report_files_min_size: None, ..WalkOptions::default() };
    let nodes = walk(Path::new(&home), &options, fs).nodes;
    World { fs, home, mounts: mac_mount_table(), nodes, snapshots: FakeSnapshots::new() }
}
