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
use crate::ports::{FileOps, Ports};

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
/// Scan every `root` the request names, each on the volume it lives on.
///
/// `broza scan <PATH>...`: a path is walked from itself down, on the volume
/// [`MountTable::volume_for`] resolves it to. A path on a volume Broza may not
/// write to, or on no known volume, is [`BrozaError::TargetNotFound`]; a
/// relative path is a usage error. Results keep the order of `roots`.
pub fn scan_paths(
    roots: &[PathBuf],
    request: &ScanRequest,
    ports: &Ports,
    mounts: &MountTable,
    progress: Option<ProgressSink<'_>>,
) -> Result<Vec<VolumeScan>, BrozaError> {
    let targets = roots
        .iter()
        .map(|root| resolve_root(root, mounts, ports.fs.as_ref()).map(|entry| (entry, root.as_path())))
        .collect::<Result<Vec<_>, BrozaError>>()?;
    let Some(sink) = progress else {
        return scan_each_root(&targets, request, ports, None);
    };
    let reporter = ProgressReporter::new(sink, ports.clock.as_ref());
    let scans = scan_each_root(&targets, request, ports, Some(&reporter));
    reporter.flush();
    scans
}

/// The mount entry a scan root belongs to, when Broza may walk it at all.
fn resolve_root<'a>(
    root: &Path,
    mounts: &'a MountTable,
    fs: &dyn FileOps,
) -> Result<&'a MountEntry, BrozaError> {
    if !root.is_absolute() {
        return Err(BrozaError::Usage(format!("{}: a scan path must be absolute", root.display())));
    }
    if !fs.exists(root) {
        return Err(BrozaError::TargetNotFound(format!("{} does not exist", root.display())));
    }
    let entry = mounts
        .volume_for(root)
        .ok_or_else(|| BrozaError::TargetNotFound(format!("{} is on no known volume", root.display())))?;
    if !entry.volume.writable_by_broza {
        return Err(BrozaError::TargetNotFound(format!(
            "{} is on {}, a volume Broza only reads about and never walks",
            root.display(),
            entry.volume.name
        )));
    }
    Ok(entry)
}

/// Scan each root, grouped by volume so that one volume's cache is loaded once
/// and written once however many roots live on it. Results keep root order;
/// the first failure in that order wins.
fn scan_each_root(
    targets: &[(&MountEntry, &Path)],
    request: &ScanRequest,
    ports: &Ports,
    reporter: Option<&ProgressReporter<'_>>,
) -> Result<Vec<VolumeScan>, BrozaError> {
    let mut groups: Vec<(&MountEntry, Vec<(usize, &Path)>)> = Vec::new();
    for (index, (entry, root)) in targets.iter().enumerate() {
        match groups.iter_mut().find(|(known, _)| known.volume.id == entry.volume.id) {
            Some((_, roots)) => roots.push((index, root)),
            None => groups.push((entry, vec![(index, root)])),
        }
    }
    let mut indexed: Vec<(usize, VolumeScan)> = groups
        .par_iter()
        .map(|(entry, roots)| scan_roots_on(entry, roots, request, ports, reporter))
        .collect::<Vec<_>>()
        .into_iter()
        .collect::<Result<Vec<_>, BrozaError>>()?
        .into_iter()
        .flatten()
        .collect();
    indexed.sort_by_key(|(index, _)| *index);
    Ok(indexed.into_iter().map(|(_, scan)| scan).collect())
}

/// The whole pipeline for one mounted volume: its cache, then every root on it.
fn scan_mounted(
    entry: &MountEntry,
    request: &ScanRequest,
    ports: &Ports,
    reporter: Option<&ProgressReporter<'_>>,
) -> Result<VolumeScan, BrozaError> {
    let mut scans = scan_roots_on(entry, &[(0, entry.mount_point.as_path())], request, ports, reporter)?;
    scans
        .pop()
        .map(|(_, scan)| scan)
        .ok_or_else(|| BrozaError::Other("a volume scan produced nothing".into()))
}

/// Walk every `root` of one volume against one loaded store, save it once.
fn scan_roots_on(
    entry: &MountEntry,
    roots: &[(usize, &Path)],
    request: &ScanRequest,
    ports: &Ports,
    reporter: Option<&ProgressReporter<'_>>,
) -> Result<Vec<(usize, VolumeScan)>, BrozaError> {
    let store_path = request.cache_root.as_ref().map(|root| cache_path_for(root, &entry.volume));
    let store = load_store(store_path.as_deref(), request, ports)?;
    let walks: Vec<(usize, &Path, WalkResult)> = roots
        .par_iter()
        .map(|(index, root)| (*index, *root, walk_volume(root, request, ports, &store, reporter)))
        .collect();
    save_store(store, store_path.as_deref(), walks.iter().map(|(_, _, walked)| walked), ports)?;
    Ok(walks
        .into_iter()
        .map(|(index, root, walked)| (index, assemble(entry, root, &walked, request)))
        .collect())
}

/// One root's walk, turned into what the report shows.
fn assemble(entry: &MountEntry, root_path: &Path, walked: &WalkResult, request: &ScanRequest) -> VolumeScan {
    let volume_id = entry.volume.id.clone();
    let root = walked.root().cloned().unwrap_or_else(|| unreadable_root(entry, root_path));
    let mut warnings = collapse_permission_warnings(walked.errors.clone(), request.verbose_warnings);
    warnings.extend(cache_key_warning(request, &entry.volume));
    warnings.extend(overcount_warning(&root, entry));
    VolumeScan {
        largest: aggregate::largest_items(walked, request.top, request.min_size, &volume_id),
        tree: aggregate::tree(&root, &walked.nodes, request.depth, request.min_size),
        root,
        warnings,
        volume_id,
        nodes: walked.nodes.clone(),
    }
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

/// How many refused paths a collapsed warning still names.
const PERMISSION_EXAMPLES: usize = 5;
/// Stable code of the one warning that stands in for many refused paths.
pub const PERMISSION_SUMMARY_CODE: &str = "permission_denied_summary";

/// Fold a flood of `permission_denied` warnings into one that counts them.
///
/// A Mac without Full Disk Access refuses hundreds of paths in one scan; one
/// line per path buries every other warning. The summary keeps the first few
/// paths as examples; `verbose` keeps them all.
fn collapse_permission_warnings(warnings: Vec<Diagnostic>, verbose: bool) -> Vec<Diagnostic> {
    let refused = warnings.iter().filter(|w| w.code == walker::PERMISSION_DENIED_CODE).count();
    if verbose || refused <= PERMISSION_EXAMPLES {
        return warnings;
    }
    let examples: Vec<String> = warnings
        .iter()
        .filter(|w| w.code == walker::PERMISSION_DENIED_CODE)
        .take(PERMISSION_EXAMPLES)
        .filter_map(|w| w.path.as_ref().map(|p| p.display().to_string()))
        .collect();
    let summary = Diagnostic {
        code: PERMISSION_SUMMARY_CODE.to_owned(),
        message: format!(
            "{refused} locations could not be read (for example {}); their sizes are missing from \
             the totals. Grant Full Disk Access to include them, or pass -v to list every path.",
            examples.join(", ")
        ),
        path: None,
    };
    std::iter::once(summary)
        .chain(warnings.into_iter().filter(|w| w.code != walker::PERMISSION_DENIED_CODE))
        .collect()
}

/// Stable code of the warning raised when a walk measures more than the volume holds.
pub const OVERCOUNT_CODE: &str = "size_exceeds_volume";

/// The warning a whole-volume walk earns when its total exceeds what macOS says
/// is in use.
///
/// Allocated blocks are summed per file, and an APFS clone reports the blocks it
/// shares with its original as its own, so a folder of cloned media can measure
/// bigger than the disk. Broza says so rather than letting the list imply more
/// space is freeable than exists (`AGENTS.md` §2.7; clone accounting is
/// post-1.0, PRD RF-02).
fn overcount_warning(root: &DirNode, entry: &MountEntry) -> Option<Diagnostic> {
    let used = entry.volume.used_bytes;
    if root.path != entry.mount_point || used == 0 || root.allocated_bytes <= used {
        return None;
    }
    Some(Diagnostic {
        code: OVERCOUNT_CODE.to_owned(),
        message: format!(
            "{} measures {} bytes but macOS reports {} in use on the volume. Each APFS clone is \
             counted separately, so the sizes listed are upper bounds until clone accounting \
             lands; the volume figure also includes snapshots and metadata a walk never sees.",
            entry.volume.name, root.allocated_bytes, used
        ),
        path: Some(entry.mount_point.clone()),
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
fn save_store<'a>(
    store: CacheStore,
    path: Option<&Path>,
    walked: impl Iterator<Item = &'a WalkResult>,
    ports: &Ports,
) -> Result<(), BrozaError> {
    let Some(path) = path else { return Ok(()) };
    // Stamped with the store's own opening instant, not the clock after the
    // walk: a walk longer than `cache-ttl` would otherwise write records the
    // store considers too new to keep, and never fill.
    let recorded_at = store.opened_at();
    let fresh = walked.flat_map(|walk| walk.nodes.iter()).filter_map(|node| DirRecord::of(node, recorded_at));
    let updated = store.with_records(fresh);
    if !updated.has_changes() {
        // Nothing the file does not already say. On a big volume this is tens
        // of megabytes of writing saved on every warm scan.
        return Ok(());
    }
    updated.save(path, ports.fs.as_ref())
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
        let reportable_inside = record.largest_item_bytes >= request.min_size;
        (record.is_usable() && !reportable_inside).then(|| record.clone())
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

/// The node reported for a root that could not be read at all.
fn unreadable_root(entry: &MountEntry, root: &Path) -> DirNode {
    DirNode {
        path: root.to_path_buf(),
        size_bytes: 0,
        allocated_bytes: 0,
        file_count: 0,
        dir_count: 0,
        dataless_count: 0,
        largest_item_bytes: 0,
        has_hard_links: false,
        has_truncation: true,
        device: entry.device,
        inode: 0,
        mtime: None,
        children_truncated: true,
        from_cache: false,
    }
}
