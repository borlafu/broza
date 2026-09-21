//! A detection world over a fake filesystem, for detector tests.
//!
//! Walks the fake home with the real scanner so detectors see exactly the nodes
//! `suggest` would hand them, then lends out a [`DetectContext`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;

use crate::detect::detector::DetectContext;
use crate::scan::{DirNode, MountTable, WalkOptions, walk};
use crate::testing::{FakeFileOps, mac_mount_table};

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
}

impl World<'_> {
    /// The context detectors are handed. `now` is fixed so "unused" is decidable.
    pub fn context(&self) -> DetectContext<'_> {
        DetectContext {
            home: &self.home,
            fs: self.fs,
            mounts: &self.mounts,
            now: "2026-09-21T10:00:00Z".parse::<Timestamp>().unwrap_or_default(),
            unused_after: Duration::from_secs(365 * 24 * 60 * 60),
            home_nodes: &self.nodes,
        }
    }
}

/// Walk `home` on `fs` and package the result.
pub fn context_over(fs: &FakeFileOps, home: PathBuf) -> World<'_> {
    let nodes = walk(Path::new(&home), &WalkOptions::default(), fs).nodes;
    World { fs, home, mounts: mac_mount_table(), nodes }
}
