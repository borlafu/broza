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
pub mod mount;
pub mod progress;
pub mod request;
pub mod selection;
pub mod walker;

pub use aggregate::{TreeNode, TreeView, largest_items, tree};
pub use cache::{CacheKey, CacheStore, DirRecord};
pub use mount::{MountEntry, MountTable};
pub use progress::{ProgressReporter, ScanProgress};
pub use request::{ScanRequest, VolumeScan, default_excludes};
pub use walker::{DirIdentity, DirNode, FileEntry, SkipHook, WalkOptions, WalkResult, walk};

use std::path::{Path, PathBuf};

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use crate::BrozaError;
use crate::model::{Diagnostic, Volume};
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
    progress: Option<ProgressSink<'_>>,
) -> Result<VolumeScan, BrozaError> {
    let wanted = request
        .volume
        .as_deref()
        .ok_or_else(|| BrozaError::Usage("scanning one volume needs --volume".to_owned()))?;
    let selected = selection::selected_volumes(mounts, request);
    let entry = selected.first().ok_or_else(|| BrozaError::TargetNotFound(format!("volume {wanted}")))?;
    let Some(sink) = progress else {
        return scan_mounted(entry, request, ports, None);
    };
    let reporter = ProgressReporter::new(sink, ports.clock.as_ref());
    let scan = scan_mounted(entry, request, ports, Some(&reporter));
    reporter.flush();
    scan
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
    let store_path = request.cache_root.as_ref().map(|root| cache_path_for(root, &entry.volume));
    let store = load_store(store_path.as_deref(), request, ports)?;
    let walked = walk_volume(&entry.mount_point, request, ports, &store, reporter);
    save_store(store, store_path.as_deref(), &walked, ports)?;
    let root = walked.root().cloned().unwrap_or_else(|| unreadable_root(entry));
    let mut warnings = walked.errors.clone();
    warnings.extend(cache_key_warning(request, &entry.volume));
    Ok(VolumeScan {
        largest: aggregate::largest_items(&walked, request.top, request.min_size, &volume_id),
        tree: aggregate::tree(&root, &walked.nodes, request.depth, request.min_size),
        root,
        warnings,
        volume_id,
    })
}

/// Where this volume's cache lives: under its UUID, or under its BSD name.
fn cache_path_for(cache_root: &Path, volume: &Volume) -> PathBuf {
    let key = volume.uuid.as_deref().unwrap_or_else(|| volume.id.as_str());
    cache::store_path(cache_root, key)
}

/// The warning a volume with no UUID earns, when a cache is in use at all.
///
/// A BSD name belongs to a slot, not to a disk: the next volume mounted there
/// would read this one's cache until it expires. Broza says so rather than
/// pretending the key is sound.
fn cache_key_warning(request: &ScanRequest, volume: &Volume) -> Option<Diagnostic> {
    if request.cache_root.is_none() || volume.uuid.is_some() {
        return None;
    }
    let message = format!(
        "macOS reported no UUID for {}, so its scan cache is filed under the BSD name {}",
        volume.name, volume.id
    );
    Some(Diagnostic { code: cache::BSD_ID_KEY_CODE.to_owned(), message, path: volume.mount_point.clone() })
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
    // A cached subtree is only reused when nothing inside it could have been
    // listed on its own; otherwise a warm scan would quietly drop an entry that
    // a cold scan shows, and the two would disagree about the same disk.
    let hook = |identity: &DirIdentity| {
        let record = CacheKey::of(identity).and_then(|key| store.lookup(&key))?;
        (record.largest_item_bytes < request.min_size).then(|| record.clone())
    };
    let options = WalkOptions {
        // The whole tree is reported: `--depth` shapes the tree view, and the
        // largest consumer of a disk is regularly deeper than two levels.
        max_depth: None,
        same_device_only: true,
        exclude: request.exclude.clone(),
        skip_hook: Some(&hook),
        // Whatever the tree view shows is measured on this run.
        cache_from_depth: request.depth.saturating_add(1),
        report_files_min_size: Some(request.min_size),
        report_files_top: request.top,
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
        dataless_count: 0,
        largest_item_bytes: 0,
        device: entry.device,
        inode: 0,
        mtime: None,
        children_truncated: true,
        from_cache: false,
    }
}
