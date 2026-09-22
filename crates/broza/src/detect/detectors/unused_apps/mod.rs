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
                bundles.push(Bundle { ids: bundle_ids(context, &entry), path: entry, removable });
            } else if let Ok(nested) = context.fs.read_dir(&entry) {
                // `/Applications/Utilities`, `/System/Applications/Utilities`: one folder deep.
                bundles.extend(nested.into_iter().filter(|nested| is_bundle(nested)).map(|nested| Bundle {
                    ids: bundle_ids(context, &nested),
                    path: nested,
                    removable,
                }));
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
/// login items), which own directories of their own under `~/Library`.
fn bundle_ids(context: &DetectContext<'_>, path: &Path) -> Vec<String> {
    let mut ids: Vec<String> = bundle_id(context, path).into_iter().collect();
    for dir in NESTED_BUNDLE_DIRS {
        let Ok(entries) = context.fs.read_dir(&path.join(dir)) else { continue };
        ids.extend(
            entries
                .iter()
                .filter(|entry| is_nested_bundle(entry))
                .filter_map(|entry| bundle_id(context, entry)),
        );
    }
    ids
}

/// `CFBundleIdentifier` of the bundle at `path`, when its manifest says.
fn bundle_id(context: &DetectContext<'_>, path: &Path) -> Option<String> {
    let manifest = path.join(INFO_PLIST);
    let size = context.fs.metadata(&manifest).ok()?.size_bytes;
    if size > MAX_INFO_PLIST_BYTES {
        return None;
    }
    let bytes = context.fs.read(&manifest).ok()?;
    let info: Info = plist::from_bytes(&bytes).ok()?;
    info.bundle_identifier.filter(|id| !id.is_empty())
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
mod tests {
    #![allow(clippy::unwrap_used)]

    use jiff::Timestamp;

    use super::*;
    use crate::detect::spotlight::{MDLS, MDLS_ARGS};
    use crate::detect::test_support::{context_over, home};
    use crate::ports::ProcessOutput;
    use crate::testing::{FakeFileOps, FakeRunner};

    pub(super) const H: &str = "/System/Volumes/Data/Users/dana";
    const OLD: &str = "/Applications/OldEditor.app";
    const FRESH: &str = "/Applications/Fresh.app";
    const MYSTERY: &str = "/Applications/Mystery.app";
    const NESTED: &str = "/Applications/Utilities/Tool.app";
    const LOCAL: &str = "/System/Volumes/Data/Users/dana/Applications/Local.app";
    const SYSTEM: &str = "/System/Applications/Mail.app";
    pub(super) const LONG_AGO: &str = "2019-08-10T14:00:00Z";
    pub(super) const LATELY: &str = "2026-09-10T08:00:00Z";

    pub(super) fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn info(id: &str) -> Vec<u8> {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{id}</string></dict></plist>"#
        )
        .into_bytes()
    }

    pub(super) fn app(fs: &FakeFileOps, path: &str, id: &str, size: u64, touched: &str) {
        fs.add_file(format!("{path}/Contents/Info.plist"), &info(id));
        fs.add_file(format!("{path}/Contents/MacOS/bin"), &[]);
        fs.set_size(format!("{path}/Contents/MacOS/bin"), size);
        fs.set_times(path, at(touched), at(touched));
    }

    /// The applications of every test, plus one leftover-looking directory
    /// per rule under `~/Library`.
    pub(super) fn fs() -> FakeFileOps {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2).with_root("/Applications", 2);
        fs.add_dir(H);
        app(&fs, OLD, "com.old.Editor", 3_000_000_000, LATELY);
        app(&fs, FRESH, "com.fresh.App", 1_000_000_000, LATELY);
        app(&fs, MYSTERY, "org.mystery.App", 500_000_000, LONG_AGO);
        app(&fs, NESTED, "com.nested.Tool", 200_000_000, LATELY);
        app(&fs, LOCAL, "com.local.App", 400_000_000, LATELY);
        app(&fs, SYSTEM, "com.apple.mail", 100_000_000, LONG_AGO);
        // A helper inside the fresh app carries its own identifier.
        fs.add_file(
            format!("{FRESH}/Contents/Frameworks/Fresh Helper.app/Contents/Info.plist"),
            &info("com.fresh.helper"),
        );
        fs
    }

    pub(super) fn answers(pairs: &[(&str, &str)]) -> FakeRunner {
        let runner = FakeRunner::new();
        for (path, answer) in pairs {
            let args: Vec<&str> = MDLS_ARGS.iter().copied().chain([*path]).collect();
            runner.script_output(
                MDLS,
                &args,
                ProcessOutput {
                    success: true,
                    code: Some(0),
                    stdout: format!("{answer}\n").into_bytes(),
                    stderr: Vec::new(),
                },
            );
        }
        runner
    }

    /// Every application opened this month, so only the leftovers are found.
    pub(super) fn everything_recent() -> FakeRunner {
        answers(&[OLD, FRESH, MYSTERY, NESTED, LOCAL].map(|app| (app, "2026-09-01 09:00:00 +0000")))
    }

    pub(super) fn detect(fs: &FakeFileOps, runner: FakeRunner) -> Detected {
        let world = context_over(fs, home()).with_process(runner);
        UnusedApps.detect(&world.context()).unwrap()
    }

    pub(super) fn finding<'a>(detected: &'a Detected, id: &str) -> Option<&'a Finding> {
        detected.findings.iter().find(|f| f.id().to_string() == format!("unused-apps.{id}"))
    }

    pub(super) fn paths(finding: Option<&Finding>) -> Vec<String> {
        finding.map(|f| f.paths().iter().map(|p| p.path.display().to_string()).collect()).unwrap_or_default()
    }

    #[test]
    fn spotlight_dates_sort_the_applications_and_a_missing_date_is_inform_only() {
        let runner = answers(&[
            (OLD, "2024-01-10 09:00:00 +0000"),
            (FRESH, "2026-09-01 09:00:00 +0000"),
            (MYSTERY, "(null)"),
            (NESTED, "2024-02-02 09:00:00 +0000"),
            (LOCAL, "2023-05-05 09:00:00 +0000"),
        ]);

        let detected = detect(&fs(), runner);

        let apps = finding(&detected, "applications");
        assert_eq!(paths(apps), vec![OLD.to_owned(), LOCAL.to_owned(), NESTED.to_owned()], "biggest first");
        assert_eq!((apps.unwrap().risk(), apps.unwrap().action()), (Risk::Amber, Action::Quarantine));
        assert_eq!(apps.unwrap().paths()[0].last_used, Some(at("2024-01-10T09:00:00Z")));
        let unverified = finding(&detected, "unverified").unwrap();
        assert_eq!(paths(Some(unverified)), vec![MYSTERY.to_owned()]);
        assert_eq!(
            (unverified.risk(), unverified.action(), unverified.is_actionable()),
            (Risk::Red, Action::InformOnly, false)
        );
        assert!(
            unverified.instructions().is_some() && unverified.reasoning().unwrap().contains("Low confidence")
        );
        assert!(paths(apps).iter().all(|p| p != SYSTEM), "Apple's are inventory only");
        assert!(detected.warnings.is_empty(), "{:?}", detected.warnings);
    }

    #[test]
    fn a_failing_mdls_is_one_warning_and_every_application_becomes_unverified() {
        let runner = FakeRunner::new();
        for path in [OLD, FRESH, MYSTERY, NESTED, LOCAL] {
            let args: Vec<&str> = MDLS_ARGS.iter().copied().chain([path]).collect();
            runner.script_failure(MDLS, &args, "mdls: command not found");
        }

        let detected = detect(&fs(), runner);

        assert!(finding(&detected, "applications").is_none());
        assert_eq!(
            paths(finding(&detected, "unverified")),
            vec![MYSTERY.to_owned()],
            "only the bundle touched long ago"
        );
        assert_eq!(detected.warnings.len(), 1, "{:?}", detected.warnings);
    }

    #[test]
    fn a_hole_inside_a_bundle_is_a_warning_and_the_application_is_still_sized() {
        let fs = fs();
        fs.add_denied(format!("{MYSTERY}/Contents/Resources"));
        fs.add_file(format!("{MYSTERY}/Contents/Resources/x"), &[]);
        let runner = answers(&[
            (MYSTERY, "(null)"),
            (OLD, "2026-09-01 09:00:00 +0000"),
            (FRESH, "2026-09-01 09:00:00 +0000"),
            (NESTED, "2026-09-01 09:00:00 +0000"),
            (LOCAL, "2026-09-01 09:00:00 +0000"),
        ]);

        let detected = detect(&fs, runner);

        assert_eq!(paths(finding(&detected, "unverified")), vec![MYSTERY.to_owned()]);
        assert!(
            detected.warnings.iter().any(|w| w.message.contains("part of this application")),
            "{:?}",
            detected.warnings
        );
    }

    #[test]
    fn a_mac_without_applications_folders_reports_nothing_and_asks_nothing() {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        fs.add_dir(H);

        let detected = detect(&fs, FakeRunner::new());

        assert!(detected.findings.is_empty() && detected.warnings.is_empty(), "{detected:?}");
    }
}
