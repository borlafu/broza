//! The detector contract and what every detector is handed.
//!
//! A detector looks at the machine and returns [`Finding`]s. It never deletes,
//! never writes, and never decides on its own what is safe: category, risk and
//! action come from the model's defaults unless the detector has a documented
//! reason to raise the risk. Every path it reports is a candidate the safety
//! kernel will check again before anything moves (`AGENTS.md` §2).

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;

use crate::BrozaError;
use crate::model::{Category, Finding};
use crate::ports::FileOps;
use crate::scan::{DirNode, MountTable};

/// One source of findings for one category.
pub trait Detector: Send + Sync {
    /// The category every finding of this detector belongs to.
    fn category(&self) -> Category;

    /// Look, and report. Failures here are turned into warnings by the
    /// registry: one detector that cannot read never hides the others.
    ///
    /// # Errors
    ///
    /// Whatever the filesystem port reports for a location the detector needs.
    fn detect(&self, context: &DetectContext<'_>) -> Result<Vec<Finding>, BrozaError>;
}

/// What a detector may look at.
///
/// The home walk is shared: every detector that needs directory sizes under the
/// home reads it instead of walking again, so `suggest` costs one walk, however
/// many detectors run (`docs/cli-spec.md` §7).
pub struct DetectContext<'a> {
    /// The user's home directory, in the spelling the walk used.
    pub home: &'a Path,
    /// Filesystem access, for what the walk does not carry (file names, contents).
    pub fs: &'a dyn FileOps,
    /// Mounted volumes, to tell a path's volume and to stay off protected ones.
    pub mounts: &'a MountTable,
    /// The instant the detection runs, for every "unused since" judgement.
    pub now: Timestamp,
    /// `--unused-after`: how long without use makes something unused.
    pub unused_after: Duration,
    /// Every directory under the home, as the scanner measured it.
    pub home_nodes: &'a [DirNode],
}

impl DetectContext<'_> {
    /// The measured node for `path`, when the walk reached it.
    pub fn node(&self, path: &Path) -> Option<&DirNode> {
        self.home_nodes.iter().find(|node| node.path == path)
    }

    /// Allocated bytes of `path` according to the walk, `0` when unknown.
    pub fn allocated_bytes(&self, path: &Path) -> u64 {
        self.node(path).map_or(0, |node| node.allocated_bytes)
    }

    /// The nodes whose final component is `name`.
    pub fn nodes_named<'b>(&'b self, name: &'b str) -> impl Iterator<Item = &'b DirNode> + 'b {
        self.home_nodes.iter().filter(move |node| node.path.file_name().is_some_and(|n| n == name))
    }

    /// The direct child directories of `path`, as the walk saw them.
    pub fn children_of<'b>(&'b self, path: &'b Path) -> impl Iterator<Item = &'b DirNode> + 'b {
        self.home_nodes.iter().filter(move |node| node.path.parent() == Some(path))
    }

    /// `true` when `moment` is at least `unused_after` before `now`.
    pub fn is_unused_since(&self, moment: Timestamp) -> bool {
        let threshold =
            jiff::SignedDuration::try_from(self.unused_after).unwrap_or(jiff::SignedDuration::MAX);
        self.now.duration_since(moment) >= threshold
    }

    /// `path` joined onto the home directory.
    pub fn under_home(&self, relative: &str) -> PathBuf {
        self.home.join(relative)
    }
}
