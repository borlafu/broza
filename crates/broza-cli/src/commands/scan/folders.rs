//! The folder half of `broza scan`: what the walker adds to the disk map.
//!
//! The enumeration says which volumes exist; this module walks the ones Broza
//! may write to (or the `PATH` arguments) and turns the result into the
//! `largest_items` of `docs/cli-spec.md` §4.2 plus the tree views the human
//! renderer draws for `--tree`. Everything the walker could not read arrives
//! as a warning, never as a failure: a partial map is still a map.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use broza::BrozaError;
use broza::model::{Disk, LargestItem, VolumeId, Warning};
use broza::ports::Ports;
use broza::safety::firmlink::firmlink_spellings;
use broza::scan::{
    CacheUse, FileReport, MountTable, ScanProgress, ScanRequest, TreeView, VolumeScan, refused_root,
    scan_all, scan_paths,
};
use broza::units::ByteSize;

use crate::args::ScanArgs;
use crate::args::scan::SCAN_DEFAULT_MIN_SIZE;

/// Where the scan cache lives, under the home directory (`docs/cli-spec.md` §7).
pub(super) const CACHE_DIR: &str = ".cache/broza";

/// What the folder scan needs from the run beyond the `scan` flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderSettings {
    /// The user's home, for the cache location and the default exclusions.
    /// `None` disables the cache and excludes nothing.
    pub home: Option<PathBuf>,
    /// `cache-ttl` from the configuration.
    pub cache_ttl: Duration,
    /// `--no-cache`.
    pub no_cache: bool,
    /// Whether to draw progress on stderr while walking.
    pub show_progress: bool,
    /// `-v`: keep every refused path instead of one counted summary.
    pub verbose: bool,
    /// Broza's own stores (scan cache, quarantine), never listed as consumers.
    pub own_stores: Vec<PathBuf>,
}

/// One volume's tree view, with the name the human renderer prints.
#[derive(Debug, Clone, PartialEq)]
pub struct VolumeTree {
    /// Volume the tree belongs to.
    pub volume_id: VolumeId,
    /// Volume name as macOS shows it.
    pub name: String,
    /// The nested view, `--depth` levels deep.
    pub tree: TreeView,
}

/// What walking produced, ready for the report and the renderer.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FolderResults {
    /// Largest items across every walked volume, biggest first.
    pub largest: Vec<LargestItem>,
    /// One tree per walked root, in volume order.
    pub trees: Vec<VolumeTree>,
    /// What could not be read.
    pub warnings: Vec<Warning>,
}

/// Walk the selected disks (or the `PATH` arguments) and gather the results.
///
/// # Errors
///
/// A `--min-size` that does not parse is a usage error; a `PATH` that is
/// relative, unknown, or on a protected volume is reported by the core
/// ([`scan_paths`]).
pub fn walk(
    disks: &[Disk],
    mounts: &MountTable,
    args: &ScanArgs,
    settings: &FolderSettings,
    ports: &Ports,
) -> Result<FolderResults, BrozaError> {
    let request = request_for(args, settings)?;
    let selected = mounts_of(disks, mounts);
    if settings.show_progress {
        super::progress::announce_first_scan(settings.home.as_deref(), settings.no_cache, ports);
    }
    let started = Instant::now();
    let show = move |progress: ScanProgress| super::progress::print(progress, started.elapsed());
    let progress = settings.show_progress.then_some(&show as &(dyn Fn(ScanProgress) + Sync));
    let scans = if args.paths.is_empty() {
        scan_all(&request, ports, &selected, progress)
    } else {
        scan_paths(&args.paths, &request, ports, &selected, progress)
    };
    if settings.show_progress {
        super::progress::clear();
    }
    let scans = refused_named_root(scans?, !args.paths.is_empty())?;
    Ok(results_of(scans, &selected, request.top))
}

/// A path the user named that macOS would not let Broza read at all makes the
/// scan impossible without the permission: exit `3` (§6). A volume-wide scan
/// whose mount point cannot be listed is an empty volume with a warning.
fn refused_named_root(scans: Vec<VolumeScan>, named: bool) -> Result<Vec<VolumeScan>, BrozaError> {
    match refused_root(&scans).filter(|_| named) {
        Some(refused) => Err(BrozaError::PermissionDenied { path: refused.to_path_buf() }),
        None => Ok(scans),
    }
}

/// The request `suggest` walks the home with: every directory measured, nothing
/// listed, the same exclusions as `scan`.
///
/// `min_size` is `0`, which keeps the cache from answering for any subtree
/// (`docs/cli-spec.md` §7): a subtree served from the cache is one node, and
/// detectors need every directory under it — the `node_modules` three levels
/// down, the `__pycache__` inside a project. The walk still refreshes the store
/// for `scan`'s benefit. Letting the cache answer for subtrees that hold none
/// of the names detectors look for is the planned way to make `suggest` warm.
///
/// # Errors
///
/// Never today; kept fallible so a future setting that can be wrong reports it.
pub fn request_for_home(settings: &FolderSettings) -> Result<ScanRequest, BrozaError> {
    let args = ScanArgs {
        paths: Vec::new(),
        depth: 0,
        top: 0,
        min_size: Some("0".to_owned()),
        volume: None,
        no_external: false,
        tree: false,
        interactive: false,
    };
    let request = request_for(&args, settings)?;
    // The detectors read directory nodes and the file report, never the
    // largest-items list, so the only floor that matters to them is the file
    // report's; setting `min_size` to it lets the cache serve every subtree
    // whose files it carries (`docs/cli-spec.md` §7).
    let report = FileReport::for_detectors();
    Ok(ScanRequest { file_report: Some(report), min_size: report.min_size, ..request })
}

/// The core request the flags and the settings add up to.
fn request_for(args: &ScanArgs, settings: &FolderSettings) -> Result<ScanRequest, BrozaError> {
    let min_size: ByteSize = args.min_size.as_deref().unwrap_or(SCAN_DEFAULT_MIN_SIZE).parse()?;
    let defaults = settings.home.as_deref().map(ScanRequest::for_home).unwrap_or_default();
    // Broza's own stores are excluded in both firmlink spellings, like the
    // cloud roots: a scan that lists its own cache or the quarantine would
    // change its own answer from one run to the next (`AGENTS.md` §2.7).
    let exclude: Vec<PathBuf> = defaults
        .exclude
        .into_iter()
        .chain(settings.own_stores.iter().flat_map(|store| firmlink_spellings(store)))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(ScanRequest {
        volume: None,
        // `select` already dropped external disks when asked; this keeps the
        // core's own filter in agreement rather than contradicting it.
        include_external: !args.no_external,
        depth: args.depth as usize,
        top: args.top as usize,
        min_size: min_size.bytes(),
        exclude,
        cache: if settings.no_cache { CacheUse::Bypass } else { CacheUse::Serve },
        cache_root: settings.home.as_deref().map(|home| home.join(CACHE_DIR)),
        cache_ttl: settings.cache_ttl,
        verbose_warnings: settings.verbose,
        file_report: None,
    })
}

/// The mount entries of the volumes the selection kept.
///
/// `select` already applied `--volume` and `--no-external` at disk level, so the
/// walker gets exactly those volumes.
fn mounts_of(disks: &[Disk], mounts: &MountTable) -> MountTable {
    let wanted: Vec<&VolumeId> =
        disks.iter().flat_map(|disk| &disk.containers).flat_map(|c| &c.volumes).map(|v| &v.id).collect();
    MountTable::new(
        mounts.entries().iter().filter(|entry| wanted.contains(&&entry.volume.id)).cloned().collect(),
    )
}

/// Fold the per-volume scans into one report, `top` items in all.
fn results_of(scans: Vec<VolumeScan>, mounts: &MountTable, top: usize) -> FolderResults {
    let mut largest: Vec<LargestItem> = scans.iter().flat_map(|scan| scan.largest.clone()).collect();
    largest.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes).then_with(|| a.path.cmp(&b.path)));
    largest.truncate(top);
    let trees = scans
        .iter()
        .map(|scan| VolumeTree {
            volume_id: scan.volume_id.clone(),
            name: volume_name(mounts, &scan.volume_id),
            tree: scan.tree.clone(),
        })
        .collect();
    let warnings = scans.into_iter().flat_map(|scan| scan.warnings).collect();
    FolderResults { largest, trees, warnings }
}

/// The name of a volume, or its id when the table does not know it.
fn volume_name(mounts: &MountTable, id: &VolumeId) -> String {
    mounts
        .entries()
        .iter()
        .find(|entry| &entry.volume.id == id)
        .map_or_else(|| id.to_string(), |entry| entry.volume.name.clone())
}

#[cfg(test)]
#[path = "folders_tests.rs"]
mod tests;
