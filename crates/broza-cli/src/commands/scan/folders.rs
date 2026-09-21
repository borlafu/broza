//! The folder half of `broza scan`: what the walker adds to the disk map.
//!
//! The enumeration says which volumes exist; this module walks the ones Broza
//! may write to (or the `PATH` arguments) and turns the result into the
//! `largest_items` of `docs/cli-spec.md` §4.2 plus the tree views the human
//! renderer draws for `--tree`. Everything the walker could not read arrives
//! as a warning, never as a failure: a partial map is still a map.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use broza::BrozaError;
use broza::model::{Disk, LargestItem, VolumeId, Warning};
use broza::ports::Ports;
use broza::scan::{MountTable, ScanProgress, ScanRequest, TreeView, VolumeScan, scan_all, scan_paths};
use broza::units::ByteSize;

use crate::args::ScanArgs;
use crate::args::scan::SCAN_DEFAULT_MIN_SIZE;
use crate::output::format_bytes;

/// Where the scan cache lives, under the home directory (`docs/cli-spec.md` §7).
const CACHE_DIR: &str = ".cache/broza";

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
    let progress = settings.show_progress.then_some(&print_progress as &(dyn Fn(ScanProgress) + Sync));
    let scans = if args.paths.is_empty() {
        scan_all(&request, ports, &selected, progress)
    } else {
        scan_paths(&args.paths, &request, ports, &selected, progress)
    };
    if settings.show_progress {
        clear_progress();
    }
    Ok(results_of(scans?, &selected))
}

/// The core request the flags and the settings add up to.
fn request_for(args: &ScanArgs, settings: &FolderSettings) -> Result<ScanRequest, BrozaError> {
    let min_size: ByteSize = args.min_size.as_deref().unwrap_or(SCAN_DEFAULT_MIN_SIZE).parse()?;
    let defaults = settings.home.as_deref().map(ScanRequest::for_home).unwrap_or_default();
    Ok(ScanRequest {
        volume: None,
        include_external: !args.no_external,
        depth: args.depth as usize,
        top: args.top as usize,
        min_size: min_size.bytes(),
        exclude: defaults.exclude,
        no_cache: settings.no_cache,
        cache_root: settings.home.as_deref().map(|home| home.join(CACHE_DIR)),
        cache_ttl: settings.cache_ttl,
    })
}

/// The mount entries of the volumes the selection kept.
///
/// `select` already applied `--volume` and `--no-external` at disk level, so the
/// walker gets exactly those volumes and no second interpretation of the flags.
fn mounts_of(disks: &[Disk], mounts: &MountTable) -> MountTable {
    let wanted: Vec<&VolumeId> =
        disks.iter().flat_map(|disk| &disk.containers).flat_map(|c| &c.volumes).map(|v| &v.id).collect();
    MountTable::new(
        mounts.entries().iter().filter(|entry| wanted.contains(&&entry.volume.id)).cloned().collect(),
    )
}

/// Fold the per-volume scans into one report.
fn results_of(scans: Vec<VolumeScan>, mounts: &MountTable) -> FolderResults {
    let mut largest: Vec<LargestItem> = scans.iter().flat_map(|scan| scan.largest.clone()).collect();
    largest.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes).then_with(|| a.path.cmp(&b.path)));
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

/// One progress line on stderr, rewritten in place.
fn print_progress(progress: ScanProgress) {
    let mut stderr = std::io::stderr();
    let _ignored = write!(
        stderr,
        "\r\x1b[2KScanning… {} entries, {}",
        progress.entries_scanned,
        format_bytes(progress.bytes_scanned)
    );
    let _ignored = stderr.flush();
}

/// Wipe the progress line before the report is printed.
fn clear_progress() {
    let mut stderr = std::io::stderr();
    let _ignored = write!(stderr, "\r\x1b[2K");
    let _ignored = stderr.flush();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::Path;

    use broza::scan::default_excludes;
    use broza::testing::mac_mount_table;

    use super::*;

    fn args() -> ScanArgs {
        ScanArgs {
            paths: Vec::new(),
            depth: 2,
            top: 20,
            min_size: None,
            volume: None,
            no_external: false,
            tree: false,
        }
    }

    fn settings() -> FolderSettings {
        FolderSettings {
            home: Some(PathBuf::from("/Users/dana")),
            cache_ttl: Duration::from_secs(60),
            no_cache: false,
            show_progress: false,
        }
    }

    #[test]
    fn the_request_carries_the_flags_the_home_and_the_cache() {
        let request = request_for(&args(), &settings()).unwrap();

        assert_eq!(request.min_size, 100_000_000, "the scan default, not the config key");
        assert_eq!(request.depth, 2);
        assert_eq!(request.top, 20);
        assert!(request.include_external);
        assert_eq!(request.exclude, default_excludes(Path::new("/Users/dana")));
        assert_eq!(request.cache_root, Some(PathBuf::from("/Users/dana/.cache/broza")));
        assert_eq!(request.cache_ttl, Duration::from_secs(60));
        assert!(request.volume.is_none(), "selection happened at disk level already");
    }

    #[test]
    fn an_explicit_min_size_is_parsed_and_a_bad_one_is_a_usage_error() {
        let good = request_for(&ScanArgs { min_size: Some("2GB".into()), ..args() }, &settings()).unwrap();
        let bad = request_for(&ScanArgs { min_size: Some("lots".into()), ..args() }, &settings()).err();

        assert_eq!(good.min_size, 2_000_000_000);
        assert!(matches!(bad, Some(BrozaError::Usage(_))), "{bad:?}");
    }

    #[test]
    fn without_a_home_there_is_no_cache_and_nothing_excluded() {
        let request = request_for(&args(), &FolderSettings { home: None, ..settings() }).unwrap();

        assert!(request.cache_root.is_none());
        assert!(request.exclude.is_empty());
    }

    #[test]
    fn only_the_selected_volumes_reach_the_walker() {
        let mounts = mac_mount_table();
        let data_only: Vec<Disk> = vec![];

        let none = mounts_of(&data_only, &mounts);

        assert!(none.entries().is_empty(), "no disks selected, nothing walked");
    }
}
