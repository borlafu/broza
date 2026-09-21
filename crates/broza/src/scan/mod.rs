//! Storage scanning: mount table, parallel walker, aggregation, cache.
//!
//! One scan of one volume is a pipeline:
//!
//! 1. resolve the volume's mount point in the [`MountTable`];
//! 2. load its [`CacheStore`], unless `--no-cache`;
//! 3. [`walk`] the mount point, letting the cache answer for subtrees whose
//!    `(device, inode, mtime)` has not changed;
//! 4. record what was freshly walked and save the store — `--no-cache` skips the
//!    load, never the save (`docs/cli-spec.md` §7);
//! 5. turn the walk into the largest items and the tree the user reads.
//!
//! [`scan_all`] runs that pipeline over every selected volume in parallel and
//! returns the results ordered by volume id, so two runs print the same thing.

pub mod aggregate;
pub mod cache;
pub mod links;
pub mod mount;
pub mod progress;
pub mod request;
pub mod selection;
pub mod walker;

pub use aggregate::{TreeNode, TreeView, largest_items, tree, usage_bar};
pub use cache::{CacheKey, CacheStore, DirRecord};
pub use mount::{MountEntry, MountTable};
pub use progress::{ProgressReporter, ScanProgress};
pub use request::{ScanRequest, VolumeScan};
pub use walker::{DirIdentity, DirNode, FileEntry, WalkOptions, WalkResult, walk};

use std::path::Path;

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use crate::BrozaError;
use crate::ports::Ports;

/// Where progress updates go while a scan runs.
pub type ProgressSink<'a> = &'a (dyn Fn(ScanProgress) + Sync);

/// Scan the one volume `request` names.
///
/// The request must name a volume (`--volume`); scanning everything is
/// [`scan_all`]. A volume that is not mounted, not writable, or simply unknown is
/// [`BrozaError::TargetNotFound`].
pub fn scan_volume(
    request: &ScanRequest,
    ports: &Ports,
    mounts: &MountTable,
) -> Result<VolumeScan, BrozaError> {
    let wanted = request
        .volume
        .as_deref()
        .ok_or_else(|| BrozaError::Usage("scanning one volume needs --volume".to_owned()))?;
    let selected = selection::selected_volumes(mounts, request);
    let entry = selected.first().ok_or_else(|| BrozaError::TargetNotFound(format!("volume {wanted}")))?;
    scan_mounted(entry, request, ports, None)
}

/// Scan every volume the request selects, in parallel, ordered by volume id.
///
/// Volumes Broza may not write to are enumerated elsewhere but never walked, and
/// `--no-external` leaves out what is mounted under `/Volumes`. A failure on one
/// volume fails the scan; what merely could not be *read* is a warning inside that
/// volume's [`VolumeScan`], never an abort.
pub fn scan_all(
    request: &ScanRequest,
    ports: &Ports,
    mounts: &MountTable,
    progress: Option<ProgressSink<'_>>,
) -> Result<Vec<VolumeScan>, BrozaError> {
    let volumes = selection::selected_volumes(mounts, request);
    let Some(sink) = progress else {
        return scan_each(&volumes, request, ports, None);
    };
    let reporter = ProgressReporter::new(sink, ports.clock.as_ref());
    let scans = scan_each(&volumes, request, ports, Some(&reporter));
    reporter.flush();
    scans
}

/// Scan each volume in parallel; the first failure in volume order wins.
fn scan_each(
    volumes: &[&MountEntry],
    request: &ScanRequest,
    ports: &Ports,
    reporter: Option<&ProgressReporter<'_>>,
) -> Result<Vec<VolumeScan>, BrozaError> {
    volumes
        .par_iter()
        .map(|entry| scan_mounted(entry, request, ports, reporter))
        .collect::<Vec<_>>()
        .into_iter()
        .collect()
}

/// The whole pipeline for one mounted volume.
fn scan_mounted(
    entry: &MountEntry,
    request: &ScanRequest,
    ports: &Ports,
    reporter: Option<&ProgressReporter<'_>>,
) -> Result<VolumeScan, BrozaError> {
    let volume_id = entry.volume.id.clone();
    let store_path = request.cache_root.as_ref().map(|root| cache::store_path(root, volume_id.as_str()));
    let store = load_store(store_path.as_deref(), request, ports)?;
    let walked = walk_volume(&entry.mount_point, request, ports, &store, reporter);
    save_store(store, store_path.as_deref(), &walked, ports)?;
    Ok(VolumeScan {
        root: walked.root().cloned().unwrap_or_else(|| unreadable_root(entry)),
        largest: aggregate::largest_items(&walked, request.top, request.min_size, &volume_id),
        tree: aggregate::tree(&walked.nodes, request.depth, request.min_size),
        warnings: walked.errors,
        volume_id,
    })
}

/// The store for this volume, or an empty one when it is disabled or bypassed.
fn load_store(path: Option<&Path>, request: &ScanRequest, ports: &Ports) -> Result<CacheStore, BrozaError> {
    let clock = ports.clock.as_ref();
    match path {
        Some(path) if !request.no_cache => {
            CacheStore::load(path, ports.fs.as_ref(), clock, request.cache_ttl)
        }
        _ => Ok(CacheStore::empty(clock, request.cache_ttl)),
    }
}

/// Fold what was freshly walked into the store and write it back.
///
/// `--no-cache` arrives here with an empty store, so the file is rewritten from
/// this walk alone: bypassing the cache still leaves a usable one behind.
fn save_store(
    store: CacheStore,
    path: Option<&Path>,
    walked: &WalkResult,
    ports: &Ports,
) -> Result<(), BrozaError> {
    let Some(path) = path else { return Ok(()) };
    let now = ports.clock.now();
    let fresh = walked.nodes.iter().filter_map(|node| DirRecord::of(node, now));
    store.with_records(fresh).save(path, ports.fs.as_ref())
}

/// Walk the volume, letting the cache answer for unchanged subtrees.
fn walk_volume(
    root: &Path,
    request: &ScanRequest,
    ports: &Ports,
    store: &CacheStore,
    reporter: Option<&ProgressReporter<'_>>,
) -> WalkResult {
    let hook = |identity: &DirIdentity| {
        CacheKey::of(identity).and_then(|key| store.lookup(&key)).map(|record| record.size_bytes)
    };
    let options = WalkOptions {
        // The whole tree is reported: `--depth` shapes the tree view, and the
        // largest consumer of a disk is regularly deeper than two levels.
        max_depth: None,
        same_device_only: true,
        exclude: request.exclude.clone(),
        skip_hook: Some(&hook),
        report_files_min_size: Some(request.min_size),
        progress: reporter,
    };
    walk(root, &options, ports.fs.as_ref())
}

/// The node reported for a volume whose own mount point could not be read.
fn unreadable_root(entry: &MountEntry) -> DirNode {
    DirNode {
        path: entry.mount_point.clone(),
        size_bytes: 0,
        allocated_bytes: 0,
        file_count: 0,
        dir_count: 0,
        device: entry.device,
        inode: 0,
        mtime: None,
        children_truncated: true,
    }
}
