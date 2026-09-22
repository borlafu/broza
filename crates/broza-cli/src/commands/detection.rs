//! The detection step `suggest` and `clean` share: one walk of the home, every
//! detector the flags allow, and the flag parsing both commands do alike.
//!
//! Both commands must see the same findings for the same machine, so the plan
//! `clean` builds is the plan `suggest` showed: detection lives in one place.

use std::path::{Path, PathBuf};
use std::time::Duration;

use broza::BrozaError;
use broza::detect::{DetectContext, DetectPorts, Registry};
use broza::model::{Category, Finding, Warning};
use broza::ports::Ports;
use broza::scan::{MountTable, VolumeScan, scan_paths};
use broza::units::{ByteSize, DurationSpec};
use jiff::Timestamp;

use crate::commands::mount::mount_table;
use crate::commands::scan::folders::{FolderSettings, request_for_home};

/// What one detection run needs.
pub struct DetectionRequest<'a> {
    /// The wired adapters.
    pub ports: &'a Ports,
    /// Home, cache and progress settings shared with `scan`.
    pub folders: &'a FolderSettings,
    /// The home directory to walk.
    pub home: &'a Path,
    /// The instant "unused since" is judged against.
    pub now: Timestamp,
    /// `--unused-after`, resolved.
    pub unused_after: Duration,
    /// `--category`, resolved; `None` runs every detector.
    pub categories: Option<&'a [Category]>,
}

/// What a detection run produced, with the mount table it used.
pub struct Detection {
    /// Every finding, in category then id order.
    pub findings: Vec<Finding>,
    /// Enumeration, mount, walk and detector warnings, in that order.
    pub warnings: Vec<Warning>,
    /// The mounted volumes, for the safety kernel downstream.
    pub mounts: MountTable,
}

/// Enumerate the disks, walk the home and run the detectors.
///
/// # Errors
///
/// Whatever enumerating the disks, reading the mount table or walking reports.
pub fn detect(request: &DetectionRequest<'_>) -> Result<Detection, BrozaError> {
    let enumeration = request.ports.disks.enumerate()?;
    let mount = mount_table(request.ports, &enumeration.disks)?;
    let walked = walk_home(request, &mount.table)?;
    let context = DetectContext::new(
        request.home,
        DetectPorts {
            fs: request.ports.fs.as_ref(),
            snapshots: request.ports.snapshots.as_ref(),
            process: request.ports.process.as_ref(),
        },
        &mount.table,
        request.now,
        request.unused_after,
        &walked.nodes,
        &walked.files,
    );
    let report = Registry::builtin().restricted_to(request.categories).run(&context);
    let warnings = [enumeration.warnings, mount.warnings, walked.warnings, report.warnings].concat();
    Ok(Detection { findings: report.findings, warnings, mounts: mount.table })
}

/// The home walk detectors read — its directories, its big files and its
/// warnings; it refreshes `scan`'s cache on the way.
fn walk_home(request: &DetectionRequest<'_>, mounts: &MountTable) -> Result<VolumeScan, BrozaError> {
    let scan_request = request_for_home(request.folders)?;
    let mut scans = scan_paths(&[request.home.to_path_buf()], &scan_request, request.ports, mounts, None)?;
    scans.pop().ok_or_else(|| BrozaError::Other("the home walk produced nothing".into()))
}

/// The home directory, or the usage error a command without one reports.
///
/// # Errors
///
/// [`BrozaError::Usage`] when `HOME` is unset.
pub fn home_of(folders: &FolderSettings) -> Result<&Path, BrozaError> {
    folders
        .home
        .as_deref()
        .ok_or_else(|| BrozaError::Usage("HOME is not set: a home directory is needed to look at".into()))
}

/// `--category` values as categories; an unknown id is a usage error.
///
/// # Errors
///
/// [`BrozaError::Usage`] naming the unknown category.
pub fn parse_categories(raw: &[String]) -> Result<Option<Vec<Category>>, BrozaError> {
    if raw.is_empty() {
        return Ok(None);
    }
    raw.iter()
        .map(|id| {
            Category::all().into_iter().find(|c| c.as_str() == id.trim()).ok_or_else(|| {
                BrozaError::Usage(format!("unknown category `{id}`; `broza explain <category>` lists them"))
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// A size flag, or the configured fallback.
///
/// # Errors
///
/// When the flag does not parse as a size.
pub fn size_or(flag: Option<&str>, fallback: ByteSize) -> Result<ByteSize, BrozaError> {
    flag.map_or(Ok(fallback), str::parse)
}

/// A duration flag, or the configured fallback.
///
/// # Errors
///
/// When the flag does not parse as a duration.
pub fn duration_or(flag: Option<&str>, fallback: DurationSpec) -> Result<Duration, BrozaError> {
    Ok(flag.map_or(Ok(fallback), str::parse::<DurationSpec>)?.to_duration())
}

/// The path of the quarantine store for `home`, from the configuration.
pub fn quarantine_root(config: &broza::config::Config, home: &Path) -> PathBuf {
    config.quarantine_dir(home)
}
