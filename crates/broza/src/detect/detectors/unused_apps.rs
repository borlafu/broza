//! `unused-apps`: applications nobody opens, and what gone ones left behind
//! (`docs/cli-spec.md` §3.3, D4).
//!
//! Three findings:
//!
//! - `unused-apps.applications` (amber, quarantine): `*.app` bundles directly
//!   under `/Applications` and `~/Applications` whose Spotlight
//!   `kMDItemLastUsedDate` — what Finder shows as "Last opened" — is older
//!   than `--unused-after`.
//! - `unused-apps.unverified` (red, quarantine): the same for bundles Spotlight
//!   has no date for, judged by the bundle's own access and modification times
//!   and stated as low confidence. Red means Broza will not act on them
//!   (`clean --apply` refuses red); the list is for the user.
//! - `unused-apps.leftovers` (amber, quarantine): directories directly under
//!   the `~/Library` locations applications write to, named by a bundle
//!   identifier (`com.vendor.App`) that no installed application carries, and
//!   untouched for `--unused-after`. The installed identifiers come from every
//!   bundle's `Contents/Info.plist` under `/Applications`, `~/Applications` and
//!   `/System/Applications`, one folder deep.
//!
//! Sizes come from the home walk where the bundle is under the home, and from
//! a measurement of the bundle for `/Applications`, candidates only. Detectors
//! never delete: the safety kernel checks every path again, and `/Applications`
//! is an allowed root for this category alone (§3.4).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::BrozaError;
use crate::model::{Category, Finding, FindingPath, Risk};
use crate::quarantine::measure_dir_bytes;

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
/// Where applications leave data behind, under `~/Library`, named by bundle id.
const LEFTOVER_DIRS: [&str; 7] = [
    "Library/Application Support",
    "Library/Caches",
    "Library/Containers",
    "Library/Saved Application State",
    "Library/Logs",
    "Library/HTTPStorages",
    "Library/WebKit",
];
/// Apple's own identifiers are never leftovers: the OS owns them.
const APPLE_PREFIX: &str = "com.apple.";
/// A bundle identifier has at least this many dot-separated labels.
const MIN_BUNDLE_LABELS: usize = 3;
/// Suffix `Saved Application State` appends to the bundle identifier.
const SAVED_STATE_SUFFIX: &str = ".savedState";

/// The `unused-apps` detector.
pub struct UnusedApps;

impl Detector for UnusedApps {
    fn category(&self) -> Category {
        Category::UnusedApps
    }

    fn detect(&self, context: &DetectContext<'_>) -> Result<Detected, BrozaError> {
        let mut detected = Detected::default();
        let bundles = bundles(context, &mut detected);
        let installed: BTreeSet<String> = bundles.iter().filter_map(|bundle| bundle.id.clone()).collect();
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
        let leftovers = leftovers(context, &installed);
        detected = detected.with_finding(applications(verified)?);
        detected = detected.with_finding(unverified_applications(unverified)?);
        detected = detected.with_finding(leftover_finding(leftovers)?);
        Ok(detected)
    }
}

/// One application bundle Broza found.
struct Bundle {
    path: PathBuf,
    /// `CFBundleIdentifier`, when the manifest could be read and names one.
    id: Option<String>,
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
                bundles.push(Bundle { id: bundle_id(context, &entry), path: entry, removable });
            } else if let Ok(nested) = context.fs.read_dir(&entry) {
                // `/Applications/Utilities`, `/System/Applications/Utilities`: one folder deep.
                bundles.extend(nested.into_iter().filter(|nested| is_bundle(nested)).map(|nested| Bundle {
                    id: bundle_id(context, &nested),
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

/// The fields of `Info.plist` Broza reads.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Info {
    #[serde(rename = "CFBundleIdentifier")]
    bundle_identifier: Option<String>,
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
        None => match measure_dir_bytes(context.fs, &bundle.path, None) {
            Ok(bytes) => path_with(&bundle.path, bytes, Some(last_used)),
            Err(error) => {
                detected.warnings.push(Detected::unreadable(
                    Category::UnusedApps,
                    &bundle.path,
                    "this application",
                    &error,
                ));
                return None;
            }
        },
    };
    Some((path, spotlight_date.is_some()))
}

/// Directories under `~/Library` named by a bundle identifier no installed
/// application carries, untouched for the threshold.
fn leftovers(context: &DetectContext<'_>, installed: &BTreeSet<String>) -> Vec<FindingPath> {
    let roots: Vec<PathBuf> = LEFTOVER_DIRS.iter().map(|dir| context.under_home(dir)).collect();
    let mut paths: Vec<FindingPath> = roots
        .iter()
        .flat_map(|root| context.children_of(root))
        .filter(|node| {
            let Some(id) = leftover_id(&node.path) else { return false };
            !installed.contains(id) && node.mtime.is_some_and(|at| context.is_unused_since(at))
        })
        .map(path_of)
        .filter(|path| path.size_bytes > 0)
        .collect();
    paths.sort_by(by_size_then_path);
    paths
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
        .description(
            "Applications Spotlight has no last-opened date for, judged by the bundle's own times alone.",
        )
        .reasoning(
            "Low confidence: macOS often records nothing for these, and an application launched by \
             another program leaves no trace here. Broza lists them and will not remove them.",
        );
    finish(builder, paths)
}

fn leftover_finding(paths: Vec<FindingPath>) -> Result<Option<Finding>, BrozaError> {
    let builder = start(Category::UnusedApps, "leftovers", "Leftovers of applications no longer installed")?
        .description(
            "Support files, caches and containers named after applications that are no longer installed.",
        )
        .reasoning(
            "No application under /Applications, ~/Applications or /System/Applications carries this bundle \
             identifier, and nothing has touched the directory for the threshold. A helper or a tool \
             installed elsewhere may still own one: read the list before applying.",
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
    use crate::model::Action;
    use crate::ports::ProcessOutput;
    use crate::testing::{FakeFileOps, FakeRunner};

    const H: &str = "/System/Volumes/Data/Users/dana";
    const OLD: &str = "/Applications/OldEditor.app";
    const FRESH: &str = "/Applications/Fresh.app";
    const MYSTERY: &str = "/Applications/Mystery.app";
    const NESTED: &str = "/Applications/Utilities/Tool.app";
    const LOCAL: &str = "/System/Volumes/Data/Users/dana/Applications/Local.app";
    const SYSTEM: &str = "/System/Applications/Mail.app";
    const LONG_AGO: &str = "2019-08-10T14:00:00Z";
    const LATELY: &str = "2026-09-10T08:00:00Z";

    fn at(text: &str) -> Timestamp {
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

    fn app(fs: &FakeFileOps, path: &str, id: &str, size: u64, touched: &str) {
        fs.add_file(format!("{path}/Contents/Info.plist"), &info(id));
        fs.add_file(format!("{path}/Contents/MacOS/bin"), &[]);
        fs.set_size(format!("{path}/Contents/MacOS/bin"), size);
        fs.set_times(path, at(touched), at(touched));
    }

    fn fs() -> FakeFileOps {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2).with_root("/Applications", 2);
        fs.add_dir(H);
        app(&fs, OLD, "com.old.Editor", 3_000_000_000, LATELY);
        app(&fs, FRESH, "com.fresh.App", 1_000_000_000, LATELY);
        app(&fs, MYSTERY, "org.mystery.App", 500_000_000, LONG_AGO);
        app(&fs, NESTED, "com.nested.Tool", 200_000_000, LATELY);
        app(&fs, LOCAL, "com.local.App", 400_000_000, LATELY);
        app(&fs, SYSTEM, "com.apple.mail", 100_000_000, LONG_AGO);
        for (dir, size, touched) in [
            ("Library/Application Support/com.gone.Tool", 900_000_000_u64, LONG_AGO),
            ("Library/Caches/com.old.Editor", 300_000_000, LONG_AGO),
            ("Library/Containers/com.apple.Safari", 800_000_000, LONG_AGO),
            ("Library/Application Support/Firefox", 700_000_000, LONG_AGO),
            ("Library/Saved Application State/net.gone.Viewer.savedState", 50_000_000, LONG_AGO),
            ("Library/Caches/com.recent.Helper", 600_000_000, LATELY),
        ] {
            let file = format!("{H}/{dir}/data.bin");
            fs.add_file(&file, &[]);
            fs.set_size(&file, size);
            fs.set_times(format!("{H}/{dir}"), at(touched), at(touched));
        }
        fs
    }

    fn answers(pairs: &[(&str, &str)]) -> FakeRunner {
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

    fn detect(fs: &FakeFileOps, runner: FakeRunner) -> Detected {
        let world = context_over(fs, home()).with_process(runner);
        UnusedApps.detect(&world.context()).unwrap()
    }

    fn finding<'a>(detected: &'a Detected, id: &str) -> Option<&'a Finding> {
        detected.findings.iter().find(|f| f.id().to_string() == format!("unused-apps.{id}"))
    }

    fn paths(finding: Option<&Finding>) -> Vec<String> {
        finding.map(|f| f.paths().iter().map(|p| p.path.display().to_string()).collect()).unwrap_or_default()
    }

    #[test]
    fn spotlight_dates_sort_the_applications_and_a_missing_date_is_low_confidence() {
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
        let unverified = finding(&detected, "unverified");
        assert_eq!(paths(unverified), vec![MYSTERY.to_owned()]);
        assert_eq!(unverified.unwrap().risk(), Risk::Red);
        assert!(unverified.unwrap().reasoning().unwrap().contains("Low confidence"));
        assert!(
            paths(apps).iter().chain(paths(unverified).iter()).all(|p| p != SYSTEM),
            "Apple's are inventory only"
        );
        assert!(detected.warnings.is_empty(), "{:?}", detected.warnings);
    }

    #[test]
    fn leftovers_are_bundle_named_directories_of_applications_nobody_has_installed() {
        let runner = answers(&[
            (OLD, "2026-09-01 09:00:00 +0000"),
            (FRESH, "2026-09-01 09:00:00 +0000"),
            (MYSTERY, "2026-09-01 09:00:00 +0000"),
            (NESTED, "2026-09-01 09:00:00 +0000"),
            (LOCAL, "2026-09-01 09:00:00 +0000"),
        ]);

        let detected = detect(&fs(), runner);

        assert!(finding(&detected, "applications").is_none(), "{detected:?}");
        let leftovers = paths(finding(&detected, "leftovers"));
        assert_eq!(
            leftovers,
            vec![
                format!("{H}/Library/Application Support/com.gone.Tool"),
                format!("{H}/Library/Saved Application State/net.gone.Viewer.savedState"),
            ],
            "installed, Apple's, plain-named and recently touched ones stay: {detected:?}"
        );
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
    fn a_mac_without_applications_folders_reports_nothing_and_asks_nothing() {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        fs.add_dir(H);

        let detected = detect(&fs, FakeRunner::new());

        assert!(detected.findings.is_empty() && detected.warnings.is_empty(), "{detected:?}");
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
}
