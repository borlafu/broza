//! `unused-apps`: applications nobody opens, and what gone ones left behind
//! (`docs/cli-spec.md` §3.3, D4).
//!
//! Three findings:
//!
//! - `unused-apps.applications` (amber, quarantine): `*.app` bundles one folder
//!   deep under `/Applications` and `~/Applications` whose Spotlight
//!   `kMDItemLastUsedDate` — what Finder shows as "Last opened" — is older
//!   than `--unused-after`.
//! - `unused-apps.unverified` (red, inform only): the same for bundles
//!   Spotlight has no date for, judged by the bundle's own access and
//!   modification times. Broza lists them with the steps to check and remove
//!   them by hand; it never acts on them, so they cost nothing in a plan and
//!   nothing in the reclaimable total.
//! - `unused-apps.leftovers` (amber, quarantine): what applications that are gone
//!   left under `~/Library`; the `leftovers` module says which locations and why.
//!
//! Sizes come from the home walk where the bundle is under the home, and from
//! a walk of the bundle for `/Applications`, candidates only. Detectors never
//! delete: the safety kernel checks every path again, and `/Applications` is
//! an allowed root for this category alone (§3.4).

mod leftovers;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::BrozaError;
use crate::model::{Action, Category, Finding, FindingPath, Instructions, Risk};
use crate::scan::{WalkOptions, walk};

use super::support::{by_size_then_path, finish, path_of, path_with, start};
use crate::detect::detector::{DetectContext, Detected, Detector};
use crate::detect::spotlight::{Spotlight, later_of};

/// Where applications the user may remove live.
const APPLICATIONS: &str = "/Applications";
/// The user's own applications folder, under the home.
const HOME_APPLICATIONS: &str = "Applications";
/// Apple's applications: consulted for installed identifiers, never proposed.
const SYSTEM_APPLICATIONS: &str = "/System/Applications";
/// What an application bundle's directory name ends with.
const APP_SUFFIX: &str = ".app";
/// The bundle's manifest, relative to the bundle.
const INFO_PLIST: &str = "Contents/Info.plist";
/// Largest `Info.plist` Broza reads: real ones are a few kilobytes.
const MAX_INFO_PLIST_BYTES: u64 = 1024 * 1024;
/// Where a bundle keeps the helpers, services and extensions that carry their
/// own identifiers and their own directories under `~/Library`.
const NESTED_BUNDLE_DIRS: [&str; 4] =
    ["Contents/Frameworks", "Contents/XPCServices", "Contents/PlugIns", "Contents/Library/LoginItems"];
/// What a nested bundle's directory name ends with.
const NESTED_BUNDLE_SUFFIXES: [&str; 3] = [".app", ".xpc", ".appex"];

/// The `unused-apps` detector.
pub struct UnusedApps;

impl Detector for UnusedApps {
    fn category(&self) -> Category {
        Category::UnusedApps
    }

    fn detect(&self, context: &DetectContext<'_>) -> Result<Detected, BrozaError> {
        let mut detected = Detected::default();
        let bundles = bundles(context, &mut detected);
        let installed: BTreeSet<String> =
            bundles.iter().flat_map(|bundle| bundle.ids.iter().cloned()).collect();
        let mut spotlight = Spotlight::new(context.process, "applications");
        let mut verified = Vec::new();
        let mut unverified = Vec::new();
        for bundle in bundles.iter().filter(|bundle| bundle.removable) {
            match judge(bundle, context, &mut spotlight, &mut detected) {
                Some((path, true)) => verified.push(path),
                Some((path, false)) => unverified.push(path),
                None => {}
            }
        }
        detected.warnings.extend(spotlight.warning());
        let leftovers = leftovers::find(context, &installed);
        detected = detected.with_finding(applications(verified)?);
        detected = detected.with_finding(unverified_applications(unverified)?);
        detected = detected.with_finding(leftovers::finding(leftovers)?);
        Ok(detected)
    }
}

/// One application bundle Broza found.
struct Bundle {
    path: PathBuf,
    /// Its `CFBundleIdentifier` and those of the helpers, services and
    /// extensions inside it, when their manifests could be read.
    ids: Vec<String>,
    /// `true` under `/Applications` or `~/Applications`; Apple's are inventory only.
    removable: bool,
}

/// Every `*.app` one folder deep under the three application folders.
fn bundles(context: &DetectContext<'_>, detected: &mut Detected) -> Vec<Bundle> {
    let folders = [
        (PathBuf::from(APPLICATIONS), true),
        (context.under_home(HOME_APPLICATIONS), true),
        (PathBuf::from(SYSTEM_APPLICATIONS), false),
    ];
    let mut bundles = Vec::new();
    for (folder, removable) in folders {
        if !context.fs.exists(&folder) {
            continue;
        }
        let entries = match context.fs.read_dir(&folder) {
            Ok(entries) => entries,
            Err(error) => {
                detected.warnings.push(Detected::unreadable(
                    Category::UnusedApps,
                    &folder,
                    "the applications",
                    &error,
                ));
                continue;
            }
        };
        for entry in entries {
            if is_bundle(&entry) {
                bundles.push(Bundle { ids: bundle_ids(context, &entry, detected), path: entry, removable });
            } else if let Ok(nested) = context.fs.read_dir(&entry) {
                // `/Applications/Utilities`, `/System/Applications/Utilities`: one folder deep.
                for nested in nested.into_iter().filter(|nested| is_bundle(nested)) {
                    bundles.push(Bundle {
                        ids: bundle_ids(context, &nested, detected),
                        path: nested,
                        removable,
                    });
                }
            }
        }
    }
    bundles.sort_by(|a, b| a.path.cmp(&b.path));
    bundles
}

fn is_bundle(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.ends_with(APP_SUFFIX))
}

fn is_nested_bundle(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| NESTED_BUNDLE_SUFFIXES.iter().any(|suffix| name.ends_with(suffix)))
}

/// The fields of `Info.plist` Broza reads.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Info {
    #[serde(rename = "CFBundleIdentifier")]
    bundle_identifier: Option<String>,
}

/// The identifiers a bundle carries: its own, and those of the bundles nested
/// one level under [`NESTED_BUNDLE_DIRS`] (helpers, XPC services, extensions,
/// login items), which own directories of their own under `~/Library`. A
/// manifest Broza cannot read is a warning: the application then credits no
/// identifier, and its leftovers would otherwise look orphaned.
fn bundle_ids(context: &DetectContext<'_>, path: &Path, detected: &mut Detected) -> Vec<String> {
    let mut candidates = vec![path.to_path_buf()];
    for dir in NESTED_BUNDLE_DIRS {
        if let Ok(entries) = context.fs.read_dir(&path.join(dir)) {
            candidates.extend(entries.into_iter().filter(|entry| is_nested_bundle(entry)));
        }
    }
    let mut ids = Vec::new();
    for bundle in candidates {
        match bundle_id(context, &bundle) {
            Ok(Some(id)) => ids.push(id),
            Ok(None) => {}
            Err(error) => detected.warnings.push(Detected::unreadable(
                Category::UnusedApps,
                &bundle,
                "this bundle's identifier",
                &error,
            )),
        }
    }
    ids
}

/// `CFBundleIdentifier` of the bundle at `path`: `None` when the manifest is
/// missing or names none, an error when it is there and cannot be read.
fn bundle_id(context: &DetectContext<'_>, path: &Path) -> Result<Option<String>, BrozaError> {
    let manifest = path.join(INFO_PLIST);
    let size = match context.fs.metadata(&manifest) {
        Ok(meta) => meta.size_bytes,
        Err(BrozaError::TargetNotFound(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    if size > MAX_INFO_PLIST_BYTES {
        return Err(BrozaError::Other(format!(
            "`{}` is {size} bytes, too big for a manifest",
            manifest.display()
        )));
    }
    let bytes = context.fs.read(&manifest)?;
    let info: Info = plist::from_bytes(&bytes).map_err(|error| {
        BrozaError::Other(format!("`{}` is not a property list: {error}", manifest.display()))
    })?;
    Ok(info.bundle_identifier.filter(|id| !id.is_empty()))
}

/// The finding path for an unused bundle and whether Spotlight vouched for it;
/// `None` when the application is in use or cannot be sized.
fn judge(
    bundle: &Bundle,
    context: &DetectContext<'_>,
    spotlight: &mut Spotlight<'_>,
    detected: &mut Detected,
) -> Option<(FindingPath, bool)> {
    let spotlight_date = spotlight.last_used(&bundle.path);
    let last_used = if let Some(opened) = spotlight_date {
        opened
    } else {
        let meta = context.fs.metadata(&bundle.path).ok()?;
        later_of(meta.accessed, meta.modified)?
    };
    if !context.is_unused_since(last_used) {
        return None;
    }
    let path = match context.node(&bundle.path) {
        Some(node) => FindingPath { last_used: Some(last_used), ..path_of(node) },
        None => path_with(&bundle.path, bundle_bytes(context, &bundle.path, detected)?, Some(last_used)),
    };
    Some((path, spotlight_date.is_some()))
}

/// Allocated bytes of a bundle outside the home, from a walk of the bundle:
/// parallel, dataless-aware, and a hole inside becomes a warning, not a lost
/// application. `None` only when the bundle itself could not be read.
fn bundle_bytes(context: &DetectContext<'_>, bundle: &Path, detected: &mut Detected) -> Option<u64> {
    let walked = walk(bundle, &WalkOptions::default(), context.fs);
    if let Some(error) = walked.errors.first() {
        detected.warnings.push(Detected::unreadable(
            Category::UnusedApps,
            error.path.as_deref().unwrap_or(bundle),
            "part of this application",
            &BrozaError::Other(error.message.clone()),
        ));
    }
    walked.root().map(|node| node.allocated_bytes)
}

fn applications(mut paths: Vec<FindingPath>) -> Result<Option<Finding>, BrozaError> {
    paths.sort_by(by_size_then_path);
    let builder = start(Category::UnusedApps, "applications", "Applications not opened in a long time")?
        .description("Applications whose last opening, as Spotlight records it, is older than the threshold.")
        .reasoning(
            "Spotlight's \"last opened\" date is what Finder shows; an application you still need is one \
             reinstall away, a licence file inside it may not be, so read the list before applying.",
        );
    finish(builder, paths)
}

fn unverified_applications(mut paths: Vec<FindingPath>) -> Result<Option<Finding>, BrozaError> {
    paths.sort_by(by_size_then_path);
    let builder = start(Category::UnusedApps, "unverified", "Applications with no record of use")?
        .risk(Risk::Red)
        .action(Action::InformOnly)
        .instructions(Instructions {
            provider: "Finder".to_owned(),
            summary: "Check these yourself: Spotlight has no record of them being opened.".to_owned(),
            steps: vec![
                "Open the application once if you still use it; Spotlight records the date and Broza \
                 stops listing it"
                    .to_owned(),
                "Otherwise drag it from /Applications to the Trash, then run `broza suggest` again for \
                 its leftovers"
                    .to_owned(),
            ],
        })
        .description(
            "Applications Spotlight has no last-opened date for, judged by the bundle's own times alone.",
        )
        .reasoning(
            "Low confidence: macOS often records nothing for these, and an application launched by \
             another program leaves no trace here. Broza lists them and will not remove them.",
        );
    finish(builder, paths)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
