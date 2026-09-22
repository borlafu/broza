//! `unused-apps.leftovers`: what applications that are gone left under
//! `~/Library`.
//!
//! A directory directly under one of the regenerable locations applications
//! write to, named by a bundle identifier (`com.vendor.App`) that no installed
//! application or any of its helpers carries, and untouched for
//! `--unused-after` by its own and its children's modification times. Only
//! `Caches`, `Logs`, `HTTPStorages` and `Saved Application State` are looked
//! at: `Application Support`, `Containers` and `Group Containers` hold data an
//! installed program may still own — a container belongs to a sandboxed app
//! and its TCC grants, a group container to a whole family — so they stay out
//! until identifiers can be matched more surely.

use std::collections::BTreeSet;
use std::path::Path;

use crate::BrozaError;
use crate::model::{Category, Finding, FindingPath};
use crate::scan::DirNode;

use super::super::support::{by_size_then_path, finish, path_of, start};
use crate::detect::detector::DetectContext;

/// Where applications leave regenerable data behind, under `~/Library`.
const LEFTOVER_DIRS: [&str; 4] =
    ["Library/Caches", "Library/Logs", "Library/HTTPStorages", "Library/Saved Application State"];
/// Apple's own identifiers are never leftovers: the OS owns them.
const APPLE_PREFIX: &str = "com.apple.";
/// A bundle identifier has at least this many dot-separated labels.
const MIN_BUNDLE_LABELS: usize = 3;
/// Suffix `Saved Application State` appends to the bundle identifier.
const SAVED_STATE_SUFFIX: &str = ".savedState";

/// The leftover directories, biggest first.
pub(super) fn find(context: &DetectContext<'_>, installed: &BTreeSet<String>) -> Vec<FindingPath> {
    let roots: Vec<std::path::PathBuf> = LEFTOVER_DIRS.iter().map(|dir| context.under_home(dir)).collect();
    let mut paths: Vec<FindingPath> = roots
        .iter()
        .flat_map(|root| context.children_of(root))
        .filter(|node| leftover_id(&node.path).is_some_and(|id| !is_installed(id, installed)))
        .filter(|node| newest_change(context, node).is_some_and(|at| context.is_unused_since(at)))
        .map(path_of)
        .filter(|path| path.size_bytes > 0)
        .collect();
    paths.sort_by(by_size_then_path);
    paths
}

/// The identifier itself, or a helper's: `com.vendor.App.helper` belongs to
/// `com.vendor.App`.
fn is_installed(id: &str, installed: &BTreeSet<String>) -> bool {
    installed.contains(id)
        || installed.iter().any(|owner| {
            id.len() > owner.len() && id.starts_with(owner) && id.as_bytes()[owner.len()] == b'.'
        })
}

/// The latest modification time of the directory and of everything directly
/// inside it, files included. A directory's own mtime moves only when a direct
/// entry is added or removed, not when a file in it is rewritten, so the
/// entries are read once. `None` when the directory cannot be listed or nothing
/// has a time: an unknown age is never "untouched".
fn newest_change(context: &DetectContext<'_>, node: &DirNode) -> Option<jiff::Timestamp> {
    let entries = context.fs.read_dir_with_metadata(&node.path).ok()?;
    entries
        .into_iter()
        .filter_map(|(_, meta)| meta.ok().and_then(|meta| meta.modified))
        .chain(context.children_of(&node.path).filter_map(|child| child.mtime))
        .chain(node.mtime)
        .max()
}

/// The bundle identifier a leftover directory is named after, when it is one.
fn leftover_id(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    let id = name.strip_suffix(SAVED_STATE_SUFFIX).unwrap_or(name);
    is_bundle_identifier(id).then_some(id)
}

/// `com.vendor.App`: three or more labels of ASCII letters, digits, `-` or `_`,
/// and not Apple's.
fn is_bundle_identifier(name: &str) -> bool {
    if name.starts_with(APPLE_PREFIX) {
        return false;
    }
    let labels: Vec<&str> = name.split('.').collect();
    labels.len() >= MIN_BUNDLE_LABELS
        && labels.iter().all(|label| {
            !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

/// The finding, or nothing when there is nothing to report.
pub(super) fn finding(paths: Vec<FindingPath>) -> Result<Option<Finding>, BrozaError> {
    let builder = start(Category::UnusedApps, "leftovers", "Leftovers of applications no longer installed")?
        .description("Caches, logs and saved state named after applications that are no longer installed.")
        .reasoning(
            "No application under /Applications, ~/Applications or /System/Applications, nor any helper \
             inside one, carries this bundle identifier, and nothing inside the directory has changed \
             for the threshold. A tool installed elsewhere may still own one: read the list before \
             applying.",
        );
    finish(builder, paths)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::super::tests::{H, LATELY, LONG_AGO, at, detect, everything_recent, finding, fs, paths};
    use super::*;

    /// A leftover directory holding one file, both last touched at `touched`.
    fn leftover(fs: &crate::testing::FakeFileOps, dir: &str, size: u64, touched: &str) {
        let file = format!("{H}/{dir}/data.bin");
        fs.add_file(&file, &[]);
        fs.set_size(&file, size);
        fs.set_times(&file, at(touched), at(touched));
        fs.set_times(format!("{H}/{dir}"), at(touched), at(touched));
    }

    #[test]
    fn leftovers_are_bundle_named_directories_of_applications_nobody_has_installed() {
        let fs = fs();
        leftover(&fs, "Library/Caches/com.gone.Tool", 900_000_000, LONG_AGO);
        leftover(&fs, "Library/Caches/com.old.Editor", 300_000_000, LONG_AGO);
        leftover(&fs, "Library/Caches/com.fresh.helper.Renderer", 250_000_000, LONG_AGO);
        leftover(&fs, "Library/Caches/com.apple.Safari", 800_000_000, LONG_AGO);
        leftover(&fs, "Library/Caches/Firefox", 700_000_000, LONG_AGO);
        leftover(&fs, "Library/Saved Application State/net.gone.Viewer.savedState", 50_000_000, LONG_AGO);
        leftover(&fs, "Library/Caches/com.recent.Helper", 600_000_000, LATELY);
        leftover(&fs, "Library/Application Support/com.gone.Tool", 950_000_000, LONG_AGO);
        // Old directory, but something inside changed lately: still in use.
        leftover(&fs, "Library/Logs/org.busy.Daemon", 400_000_000, LONG_AGO);
        fs.add_file(format!("{H}/Library/Logs/org.busy.Daemon/today/log.txt"), b"x");
        fs.set_times(format!("{H}/Library/Logs/org.busy.Daemon/today"), at(LATELY), at(LATELY));
        // Old directory whose one log file is rewritten in place: still in use too.
        leftover(&fs, "Library/Logs/org.chatty.Agent", 400_000_000, LONG_AGO);
        fs.set_times(format!("{H}/Library/Logs/org.chatty.Agent/data.bin"), at(LATELY), at(LATELY));

        let detected = detect(&fs, everything_recent());

        assert!(finding(&detected, "applications").is_none(), "{detected:?}");
        assert_eq!(
            paths(finding(&detected, "leftovers")),
            vec![
                format!("{H}/Library/Caches/com.gone.Tool"),
                format!("{H}/Library/Saved Application State/net.gone.Viewer.savedState"),
            ],
            "installed, helpers of installed, Apple's, plain-named, recently touched, busy inside, \
             rewritten in place and Application Support entries stay: {detected:?}"
        );
    }

    #[test]
    fn bundle_identifiers_are_reverse_dns_and_never_apples() {
        assert!(is_bundle_identifier("com.vendor.App"));
        assert!(is_bundle_identifier("org.some-vendor.App_2"));
        assert!(!is_bundle_identifier("com.apple.Safari"));
        assert!(!is_bundle_identifier("Firefox"));
        assert!(!is_bundle_identifier("com.vendor"));
        assert!(!is_bundle_identifier("com..App"));
        assert!(!is_bundle_identifier("com.vendor.App Support"));
    }

    #[test]
    fn a_helpers_identifier_belongs_to_the_application_that_carries_it() {
        let installed: BTreeSet<String> = ["com.vendor.App".to_owned()].into_iter().collect();
        assert!(is_installed("com.vendor.App", &installed));
        assert!(is_installed("com.vendor.App.helper", &installed));
        assert!(!is_installed("com.vendor.AppSupport", &installed), "a longer name is not a helper");
        assert!(!is_installed("com.vendor", &installed));
    }
}
