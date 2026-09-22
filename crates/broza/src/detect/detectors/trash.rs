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
    for entry in context.mounts.entries().iter().filter(|entry| holds_a_trash(entry, context.home)) {
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
            // Another user's trash is not readable and not ours to empty.
            if let Some(found) = list_trash(context, &user_trash, &mut Detected::default(), "") {
                paths.extend(found);
            }
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
fn holds_a_trash(entry: &MountEntry, home: &Path) -> bool {
    entry.volume.writable_by_broza
        && !is_protected(entry.volume.role)
        && !home.starts_with(&entry.mount_point)
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
    let mut paths: Vec<FindingPath> = listing
        .into_iter()
        .filter_map(|(path, meta)| meta.ok().map(|meta| (path, meta)))
        .map(|(path, meta)| {
            let bytes = if meta.is_dir { dir_bytes(context, &path) } else { meta.allocated_bytes };
            path_with(&path, bytes, meta.modified)
        })
        .collect();
    paths.sort_by(by_size_then_path);
    Some(paths)
}

/// Allocated bytes of a directory: the walk's figure, or a direct measurement.
fn dir_bytes(context: &DetectContext<'_>, path: &Path) -> u64 {
    context
        .node(path)
        .map(|node| node.allocated_bytes)
        .or_else(|| measure_dir_bytes(context.fs, path, None).ok())
        .unwrap_or(0)
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
    fn an_empty_trash_yields_no_finding() {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2).with_root("/Volumes/External", 6);
        fs.add_dir(format!("{H}/.Trash"));
        fs.add_dir(format!("{H}/Documents"));

        let detected = detect(&fs);

        assert!(detected.findings.is_empty(), "{:?}", detected.findings);
    }
}
