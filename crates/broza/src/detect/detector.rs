//! The detector contract and what every detector is handed.
//!
//! A detector looks at the machine and returns [`Finding`]s. It never deletes,
//! never writes, and never decides on its own what is safe: category, risk and
//! action come from the model's defaults unless the detector has a documented
//! reason to raise the risk. Every path it reports is a candidate the safety
//! kernel will check again before anything moves (`AGENTS.md` §2).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;

use crate::BrozaError;
use crate::model::{Category, Diagnostic, Finding};
use crate::ports::{FileOps, ProcessRunner, SnapshotProvider};
use crate::scan::{DirNode, MountTable};

/// Warning code for a location one detector wanted and could not read.
///
/// The detector's other findings stand; only the part that needed this
/// location is missing, and the warning says which.
pub const LOCATION_UNREADABLE_CODE: &str = "location_unreadable";

/// One source of findings for one category.
pub trait Detector: Send + Sync {
    /// The category every finding of this detector belongs to.
    fn category(&self) -> Category;

    /// Look, and report.
    ///
    /// A location the detector cannot read is a warning inside [`Detected`],
    /// not an error: `~/Downloads` being off limits must not hide the caches.
    /// The registry turns an `Err` into a `detector_failed` warning, so even a
    /// programming error in one detector never hides the others.
    ///
    /// # Errors
    ///
    /// Only for failures that leave the detector with nothing to say.
    fn detect(&self, context: &DetectContext<'_>) -> Result<Detected, BrozaError>;
}

/// What one detector returned: its findings and what it could not look at.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Detected {
    /// Findings, in the detector's own order.
    pub findings: Vec<Finding>,
    /// Locations that could not be read, one warning each.
    pub warnings: Vec<Diagnostic>,
}

impl Detected {
    /// Add a finding when there is one.
    #[must_use]
    pub fn with_finding(mut self, finding: Option<Finding>) -> Self {
        self.findings.extend(finding);
        self
    }

    /// Fold another result into this one, in order.
    #[must_use]
    pub fn merged(mut self, other: Self) -> Self {
        self.findings.extend(other.findings);
        self.warnings.extend(other.warnings);
        self
    }

    /// The warning for a location `category`'s detector could not read.
    pub fn unreadable(category: Category, path: &Path, what: &str, error: &BrozaError) -> Diagnostic {
        Diagnostic {
            code: LOCATION_UNREADABLE_CODE.to_owned(),
            message: format!(
                "the {} detector could not read {}: {}; {what} were not checked",
                category.as_str(),
                path.display(),
                first_line(&error.to_string())
            ),
            path: Some(path.to_path_buf()),
        }
    }
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
    /// APFS local snapshots, for the `snapshots` detector.
    pub snapshots: &'a dyn SnapshotProvider,
    /// External commands (`xcrun simctl`, `mdls`), always with a timeout.
    pub process: &'a dyn ProcessRunner,
    /// The same nodes, indexed by path and by parent.
    index: NodeIndex<'a>,
}

/// Lookups over the walk that would otherwise be linear scans of every node.
///
/// Paths and parents are indexed; names are not. Detectors ask for a handful of
/// fixed names, and one pass over the nodes per name is cheaper than a vector
/// per distinct directory name on a home with a million of them.
struct NodeIndex<'a> {
    by_path: HashMap<&'a Path, &'a DirNode>,
    by_parent: HashMap<&'a Path, Vec<&'a DirNode>>,
}

impl<'a> NodeIndex<'a> {
    fn of(nodes: &'a [DirNode]) -> Self {
        let by_path = nodes.iter().map(|node| (node.path.as_path(), node)).collect();
        let by_parent = nodes.iter().filter_map(|node| node.path.parent().map(|parent| (parent, node))).fold(
            HashMap::<&Path, Vec<&DirNode>>::new(),
            |mut map, (parent, node)| {
                map.entry(parent).or_default().push(node);
                map
            },
        );
        Self { by_path, by_parent }
    }
}

/// The adapters a detection run reads through.
#[derive(Clone, Copy)]
pub struct DetectPorts<'a> {
    /// Filesystem access, for what the walk does not carry.
    pub fs: &'a dyn FileOps,
    /// APFS local snapshots.
    pub snapshots: &'a dyn SnapshotProvider,
    /// External commands, always with a timeout.
    pub process: &'a dyn ProcessRunner,
}

impl<'a> DetectContext<'a> {
    /// Package what detectors read, indexing the walk once.
    pub fn new(
        home: &'a Path,
        ports: DetectPorts<'a>,
        mounts: &'a MountTable,
        now: Timestamp,
        unused_after: Duration,
        home_nodes: &'a [DirNode],
    ) -> Self {
        Self {
            home,
            fs: ports.fs,
            mounts,
            now,
            unused_after,
            home_nodes,
            snapshots: ports.snapshots,
            process: ports.process,
            index: NodeIndex::of(home_nodes),
        }
    }

    /// The measured node for `path`, when the walk reached it.
    pub fn node(&self, path: &Path) -> Option<&'a DirNode> {
        self.index.by_path.get(path).copied()
    }

    /// Allocated bytes of `path` according to the walk, `0` when unknown.
    pub fn allocated_bytes(&self, path: &Path) -> u64 {
        self.node(path).map_or(0, |node| node.allocated_bytes)
    }

    /// The nodes whose final component is `name`, in walk order.
    pub fn nodes_named<'b>(&'b self, name: &'b str) -> impl Iterator<Item = &'a DirNode> + 'b {
        let wanted = OsStr::new(name);
        self.home_nodes.iter().filter(move |node| node.path.file_name() == Some(wanted))
    }

    /// The direct child directories of `path`, as the walk saw them.
    pub fn children_of(&self, path: &Path) -> impl Iterator<Item = &'a DirNode> + '_ {
        self.index.by_parent.get(path).into_iter().flatten().copied()
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

/// Longest a reason quoted in a warning gets: one line, bounded.
const REASON_MAX_CHARS: usize = 200;

/// The first line of an error, cut to [`REASON_MAX_CHARS`]: a warning quotes
/// the reason, it does not reproduce a dump.
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() <= REASON_MAX_CHARS {
        return line.to_owned();
    }
    let cut: String = line.chars().take(REASON_MAX_CHARS).collect();
    format!("{cut}…")
}
