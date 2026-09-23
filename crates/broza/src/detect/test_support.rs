//! A detection world over a fake filesystem, for detector tests.
//!
//! Walks the fake home with the real scanner so detectors see exactly the nodes
//! `suggest` would hand them, then lends out a [`DetectContext`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;

use crate::detect::detector::{DetectContext, DetectPorts, HomeWalk};
use crate::scan::{DirNode, FileEntry, FileReport, MountTable, WalkOptions, default_excludes, walk};
use crate::testing::{FakeFileOps, FakeRunner, FakeSnapshots, mac_mount_table};

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
    files: Vec<FileEntry>,
    clone_families: Vec<(u64, u64)>,
    snapshots: FakeSnapshots,
    process: FakeRunner,
}

impl World<'_> {
    /// The context detectors are handed. `now` is fixed so "unused" is decidable.
    pub fn context(&self) -> DetectContext<'_> {
        DetectContext::new(
            &self.home,
            DetectPorts { fs: self.fs, snapshots: &self.snapshots, process: &self.process },
            &self.mounts,
            "2026-09-21T10:00:00Z".parse::<Timestamp>().unwrap_or_default(),
            Duration::from_secs(365 * 24 * 60 * 60),
            HomeWalk { nodes: &self.nodes, files: &self.files, clone_families: &self.clone_families },
        )
    }

    /// The same world answering external commands with `runner`.
    #[must_use]
    pub fn with_process(self, runner: FakeRunner) -> Self {
        Self { process: runner, ..self }
    }

    /// The scripted process runner, to assert on what was asked of it.
    pub fn process(&self) -> &FakeRunner {
        &self.process
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
    let report = FileReport::for_detectors();
    let options = WalkOptions {
        exclude,
        report_files_min_size: Some(report.min_size),
        report_files_top: report.top,
        ..WalkOptions::default()
    };
    let walked = walk(Path::new(&home), &options, fs);
    World {
        fs,
        home,
        mounts: mac_mount_table(),
        nodes: walked.nodes,
        files: walked.files,
        clone_families: walked.clone_families,
        snapshots: FakeSnapshots::new(),
        process: FakeRunner::new(),
    }
}
