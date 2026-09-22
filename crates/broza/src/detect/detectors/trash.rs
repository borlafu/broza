//! `trash`: what the user already threw away (`docs/cli-spec.md` §3.3).
//!
//! Two findings, both amber and both `purge` — emptying a trash is irreversible
//! by nature, so the plan says so and the confirmation is the detailed one
//! (`docs/cli-spec.md` §3.4):
//!
//! - `trash.home` — the direct children of `~/.Trash`, sized from the home walk;
//! - `trash.external-volumes` — the direct children of `<mount>/.Trashes/<uid>`
//!   on every mounted volume Broza may write to other than the one the home is
//!   on, measured directly. A trash directory that belongs to another user is
//!   not readable and is passed over; a `.Trashes` Broza cannot read at all is a
//!   `location_unreadable` warning.

use std::path::Path;

use crate::BrozaError;
use crate::model::{Category, FindingPath};
use crate::quarantine::measure_dir_bytes;
use crate::safety::roles::is_protected;
use crate::scan::MountEntry;

use super::support::{by_size_then_path, finish, path_with, start};
use crate::detect::detector::{DetectContext, Detected, Detector};

/// The user's own trash, under the home.
const HOME_TRASH: &str = ".Trash";
/// Per-volume trash directory, holding one subdirectory per uid.
const VOLUME_TRASHES: &str = ".Trashes";

/// The `trash` detector.
pub struct Trash;

impl Detector for Trash {
    fn category(&self) -> Category {
        Category::Trash
    }

    fn detect(&self, context: &DetectContext<'_>) -> Result<Detected, BrozaError> {
        let home = home_trash(context)?;
        let external = external_trashes(context)?;
        Ok(home.merged(external))
    }
}

/// `~/.Trash`, one path per item in it.
fn home_trash(context: &DetectContext<'_>) -> Result<Detected, BrozaError> {
    let trash = context.under_home(HOME_TRASH);
    let mut detected = Detected::default();
    let Some(paths) = list_trash(context, &trash, &mut detected, "your Trash") else {
        return Ok(detected);
    };
    let builder = start(Category::Trash, "home", "Trash")?
        .description("Items you moved to the Trash and have not emptied.")
        .reasoning(
            "Already thrown away by you; emptying the Trash deletes them for good, with no quarantine.",
        );
    Ok(detected.with_finding(finish(builder, paths)?))
}

/// `<mount>/.Trashes/<uid>/*` on every other writable volume.
fn external_trashes(context: &DetectContext<'_>) -> Result<Detected, BrozaError> {
    let mut detected = Detected::default();
    let mut paths = Vec::new();
    // The home's volume is found through the mount table, firmlinks included:
    // `/Users/dana` and `/System/Volumes/Data/Users/dana` are the same volume.
    let home_device = context.mounts.volume_for(context.home).map(|entry| entry.device);
    for entry in context.mounts.entries().iter().filter(|entry| holds_a_trash(entry, home_device)) {
        let trashes = entry.mount_point.join(VOLUME_TRASHES);
        if !context.fs.exists(&trashes) {
            continue;
        }
        let per_user = match context.fs.read_dir(&trashes) {
            Ok(children) => children,
            Err(error) => {
                detected.warnings.push(Detected::unreadable(
                    Category::Trash,
                    &trashes,
                    "that volume's trash",
                    &error,
                ));
                continue;
            }
        };
        for user_trash in per_user {
            // Another user's trash is not readable and not ours to empty; a
            // folder inside a readable one that cannot be measured is warned about.
            let mut inner = Detected::default();
            if let Some(found) = list_trash(context, &user_trash, &mut inner, "") {
                paths.extend(found);
            }
            detected.warnings.extend(inner.warnings);
        }
    }
    paths.sort_by(by_size_then_path);
    let builder = start(Category::Trash, "external-volumes", "Trash on other volumes")?
        .description(
            "Items thrown away on external or secondary volumes; the Finder empties these with your Trash.",
        )
        .reasoning(
            "Already thrown away by you; emptying the Trash deletes them for good, with no quarantine.",
        );
    Ok(detected.with_finding(finish(builder, paths)?))
}

/// `true` for a mounted volume whose `.Trashes` Broza may empty: writable,
/// unprotected, and not the volume the home lives on (that one has `~/.Trash`).
/// A home whose volume is unknown keeps every volume's `.Trashes` off the list.
fn holds_a_trash(entry: &MountEntry, home_device: Option<u64>) -> bool {
    entry.volume.writable_by_broza
        && !is_protected(entry.volume.role)
        && home_device.is_some_and(|device| device != entry.device)
}

/// The items directly inside `trash`, sized: files from `lstat`, directories
/// from the walk when it covered them and by measuring otherwise.
///
/// `None` when the directory does not exist or cannot be read; the latter adds
/// a warning to `detected` when `what` names the location.
fn list_trash(
    context: &DetectContext<'_>,
    trash: &Path,
    detected: &mut Detected,
    what: &str,
) -> Option<Vec<FindingPath>> {
    if !context.fs.exists(trash) {
        return None;
    }
    let listing = match context.fs.read_dir_with_metadata(trash) {
        Ok(listing) => listing,
        Err(error) => {
            if !what.is_empty() {
                detected.warnings.push(Detected::unreadable(Category::Trash, trash, what, &error));
            }
            return None;
        }
    };
    let mut paths: Vec<FindingPath> = Vec::new();
    for (path, meta) in listing {
        let meta = match meta {
            Ok(meta) => meta,
            Err(error) => {
                detected.warnings.push(Detected::unreadable(
                    Category::Trash,
                    &path,
                    "this trashed item",
                    &error,
                ));
                continue;
            }
        };
        if !meta.is_dir {
            paths.push(path_with(&path, meta.allocated_bytes, meta.modified));
            continue;
        }
        // A directory Broza cannot measure is not listed at zero bytes: that
        // would plan a purge the `--max-size` cap could never see.
        match dir_bytes(context, &path) {
            Ok(bytes) => paths.push(path_with(&path, bytes, meta.modified)),
            Err(error) => {
                detected.warnings.push(Detected::unreadable(
                    Category::Trash,
                    &path,
                    "this trashed folder",
                    &error,
                ));
            }
        }
    }
    paths.sort_by(by_size_then_path);
    Some(paths)
}

/// Allocated bytes of a directory: the walk's figure, or a direct measurement.
fn dir_bytes(context: &DetectContext<'_>, path: &Path) -> Result<u64, BrozaError> {
    match context.node(path) {
        Some(node) => Ok(node.allocated_bytes),
        None => measure_dir_bytes(context.fs, path, None),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::detect::detector::LOCATION_UNREADABLE_CODE;
    use crate::detect::test_support::{context_over, home};
    use crate::model::{Action, Finding, Risk};
    use crate::testing::FakeFileOps;

    const H: &str = "/System/Volumes/Data/Users/dana";

    fn fs() -> FakeFileOps {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2).with_root("/Volumes/External", 6);
        for (path, size) in [
            (format!("{H}/.Trash/old-movie.mp4"), 40_000_u64),
            (format!("{H}/.Trash/Project/src/main.rs"), 10_000),
            (format!("{H}/.Trash/Project/README"), 1000),
            (format!("{H}/Documents/keep.txt"), 500),
            ("/Volumes/External/.Trashes/501/dump.iso".to_owned(), 90_000),
            ("/Volumes/External/.Trashes/502/theirs.bin".to_owned(), 70_000),
            ("/Volumes/External/photos/a.jpg".to_owned(), 5000),
        ] {
            fs.add_file(&path, &[]);
            fs.set_size(&path, size);
        }
        fs.add_denied("/Volumes/External/.Trashes/502");
        fs
    }

    fn detect(fs: &FakeFileOps) -> Detected {
        let world = context_over(fs, home());
        Trash.detect(&world.context()).unwrap_or_else(|e| panic!("{e}"))
    }

    fn by_id<'a>(detected: &'a Detected, id: &str) -> &'a Finding {
        detected
            .findings
            .iter()
            .find(|f| f.id().to_string() == id)
            .unwrap_or_else(|| panic!("{id}: {detected:?}"))
    }

    fn listed(finding: &Finding) -> Vec<String> {
        finding.paths().iter().map(|p| p.path.display().to_string()).collect()
    }

    #[test]
    fn the_home_trash_lists_its_direct_children_biggest_first_as_a_purge() {
        let detected = detect(&fs());

        let trash = by_id(&detected, "trash.home");
        assert_eq!(listed(trash), vec![format!("{H}/.Trash/old-movie.mp4"), format!("{H}/.Trash/Project")]);
        assert_eq!(trash.action(), Action::Purge);
        assert_eq!(trash.risk(), Risk::Amber);
        assert_eq!(
            trash.reclaimable_bytes(),
            10 * 4096 + 3 * 4096 + 4096,
            "file blocks plus the walked directory"
        );
        assert!(detected.warnings.is_empty(), "{:?}", detected.warnings);
    }

    #[test]
    fn other_volumes_contribute_their_readable_trash_and_skip_other_users() {
        let detected = detect(&fs());

        let external = by_id(&detected, "trash.external-volumes");
        assert_eq!(listed(external), vec!["/Volumes/External/.Trashes/501/dump.iso"]);
        assert!(detected.warnings.is_empty(), "another user's trash is passed over quietly");
    }

    #[test]
    fn an_unreadable_trashes_directory_is_a_warning_not_a_failure() {
        let fs = fs();
        fs.add_denied("/Volumes/External/.Trashes");

        let detected = detect(&fs);

        assert!(detected.findings.iter().any(|f| f.id().to_string() == "trash.home"));
        assert!(detected.findings.iter().all(|f| f.id().to_string() != "trash.external-volumes"));
        assert_eq!(detected.warnings.len(), 1, "{:?}", detected.warnings);
        assert_eq!(detected.warnings[0].code, LOCATION_UNREADABLE_CODE);
    }

    #[test]
    fn the_data_volume_trashes_are_never_listed_whatever_spelling_the_home_uses() {
        let data_device = crate::testing::mac_mount_table()
            .volume_for(Path::new("/Users/dana"))
            .map_or_else(|| panic!("the fake table knows the Data volume"), |entry| entry.device);
        let fs = FakeFileOps::new()
            .with_root("/", 1)
            .with_root("/Users", data_device)
            .with_root("/System/Volumes/Data", data_device)
            .with_root("/Volumes/External", 6);
        for path in [
            "/Users/dana/.Trash/mine.bin",
            "/System/Volumes/Data/.Trashes/501/not-mine.bin",
            "/Volumes/External/.Trashes/501/ext.bin",
        ] {
            fs.add_file(path, &[]);
            fs.set_size(path, 9000);
        }
        let world = context_over(&fs, std::path::PathBuf::from("/Users/dana"));

        let detected = Trash.detect(&world.context()).unwrap_or_else(|e| panic!("{e}"));

        let external = by_id(&detected, "trash.external-volumes");
        assert_eq!(listed(external), vec!["/Volumes/External/.Trashes/501/ext.bin"], "{detected:?}");
        assert_eq!(listed(by_id(&detected, "trash.home")), vec!["/Users/dana/.Trash/mine.bin"]);
    }

    #[test]
    fn a_trashed_folder_that_cannot_be_measured_is_a_warning_not_a_zero_byte_purge() {
        let fs = fs();
        fs.add_file(format!("{H}/.Trash/Locked/secret"), &[]);
        fs.add_denied(format!("{H}/.Trash/Locked"));

        let detected = detect(&fs);

        let trash = by_id(&detected, "trash.home");
        assert!(listed(trash).iter().all(|p| !p.ends_with("Locked")), "{:?}", listed(trash));
        assert!(
            detected.warnings.iter().any(|w| w.code == LOCATION_UNREADABLE_CODE),
            "{:?}",
            detected.warnings
        );
    }

    #[test]
    fn an_empty_trash_yields_no_finding() {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2).with_root("/Volumes/External", 6);
        fs.add_dir(format!("{H}/.Trash"));
        fs.add_dir(format!("{H}/Documents"));

        let detected = detect(&fs);

        assert!(detected.findings.is_empty(), "{:?}", detected.findings);
    }
}
