//! `cloud-synced`: local copies a cloud provider already holds
//! (`docs/cli-spec.md` §3.3, RF-19, D3).
//!
//! One finding per provider present, always red and `inform_only`: Broza never
//! deletes a synced file, because deleting it locally deletes it from the cloud
//! and from every other device. What it reports is the space the provider
//! could give back by evicting local copies, and the provider's own steps for
//! asking it to.
//!
//! The roots are iCloud Drive (`~/Library/Mobile Documents`), every File
//! Provider root under `~/Library/CloudStorage` (grouped by the name's prefix:
//! `Dropbox-…`, `OneDrive-…`, `GoogleDrive-…`, anything else is "other cloud
//! storage"), and the legacy `~/Dropbox`, `~/OneDrive` and `~/Google Drive`.
//! The home walk leaves these roots out; this detector walks each one itself,
//! with the walker, which never descends into a dataless directory and counts a
//! dataless file as 0 bytes, so it measures the disk and not the network. A
//! root that cannot be read is a `location_unreadable` warning.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::model::{Category, FindingPath, Instructions};
use crate::scan::{WalkOptions, walk};

use super::support::{by_size_then_path, start};
use crate::detect::detector::{DetectContext, Detected, Detector};

/// iCloud Drive and every iCloud-backed app container.
const ICLOUD_ROOT: &str = "Library/Mobile Documents";
/// Where File Provider extensions mount their roots, one per account.
const CLOUD_STORAGE_DIR: &str = "Library/CloudStorage";
/// Sync clients from before File Provider put their folder in the home.
const LEGACY_ROOTS: [(&str, Provider); 3] = [
    ("Dropbox", Provider::Dropbox),
    ("OneDrive", Provider::OneDrive),
    ("Google Drive", Provider::GoogleDrive),
];

/// A cloud provider Broza knows the official eviction steps of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Provider {
    ICloud,
    Dropbox,
    OneDrive,
    GoogleDrive,
    Other,
}

impl Provider {
    /// The provider a `~/Library/CloudStorage` entry belongs to, by its prefix.
    fn of_storage_entry(name: &str) -> Self {
        if name.starts_with("Dropbox") {
            Self::Dropbox
        } else if name.starts_with("OneDrive") {
            Self::OneDrive
        } else if name.starts_with("GoogleDrive") {
            Self::GoogleDrive
        } else {
            Self::Other
        }
    }

    /// The finding's detector name: `cloud-synced.<this>`.
    fn slug(self) -> &'static str {
        match self {
            Self::ICloud => "icloud",
            Self::Dropbox => "dropbox",
            Self::OneDrive => "onedrive",
            Self::GoogleDrive => "google-drive",
            Self::Other => "other",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::ICloud => "iCloud Drive",
            Self::Dropbox => "Dropbox",
            Self::OneDrive => "OneDrive",
            Self::GoogleDrive => "Google Drive",
            Self::Other => "other cloud storage",
        }
    }

    /// The provider's own way of releasing local copies.
    fn instructions(self) -> Instructions {
        let (summary, steps): (&str, &[&str]) = match self {
            Self::ICloud => (
                "Use Apple's official feature to release local copies.",
                &[
                    "System Settings → [your name] → iCloud → iCloud Drive",
                    "Turn on \"Optimize Mac Storage\"",
                    "For one file or folder: right-click it in Finder → \"Remove Download\"",
                ],
            ),
            Self::Dropbox => (
                "Ask Dropbox to keep the files online-only.",
                &[
                    "Right-click the file or folder in Finder → \"Make Available Online Only\"",
                    "Dropbox → Preferences → Sync → set new files to \"Online-only\"",
                ],
            ),
            Self::OneDrive => (
                "Ask OneDrive to free up the local copies with Files On-Demand.",
                &[
                    "OneDrive → Preferences → Files On-Demand → \"Free up space\"",
                    "For one file or folder: right-click it in Finder → \"Free Up Space\"",
                ],
            ),
            Self::GoogleDrive => (
                "Ask Google Drive to stream the files instead of mirroring them.",
                &[
                    "Google Drive → Preferences → Google Drive → \"Stream files\"",
                    "For one file or folder: right-click it in Finder → Offline access → \"Available online only\"",
                ],
            ),
            Self::Other => (
                "Use the provider's own setting to keep the files online-only.",
                &[
                    "Open the provider's preferences and look for \"online-only\", \"on demand\" or \"free up space\"",
                ],
            ),
        };
        Instructions {
            provider: self.display_name().to_owned(),
            summary: summary.to_owned(),
            steps: steps.iter().map(|step| (*step).to_owned()).collect(),
        }
    }
}

/// One measured root: what is on this disk, and how many local files.
struct Measured {
    path: FindingPath,
    local_files: u64,
}

/// The `cloud-synced` detector.
pub struct CloudSynced;

impl Detector for CloudSynced {
    fn category(&self) -> Category {
        Category::CloudSynced
    }

    fn detect(&self, context: &DetectContext<'_>) -> Result<Detected, BrozaError> {
        let mut detected = Detected::default();
        let mut by_provider: BTreeMap<Provider, Vec<Measured>> = BTreeMap::new();
        for (provider, root) in roots(context, &mut detected) {
            match measure(context, &root) {
                Ok(Some(measured)) => by_provider.entry(provider).or_default().push(measured),
                Ok(None) => {}
                Err(error) => detected.warnings.push(Detected::unreadable(
                    Category::CloudSynced,
                    &root,
                    "its synced files",
                    &error,
                )),
            }
        }
        for (provider, measured) in by_provider {
            detected = detected.with_finding(Some(finding(provider, measured)?));
        }
        Ok(detected)
    }
}

/// Every cloud root present under the home, with its provider.
///
/// A root is a directory the provider owns; a file at one of these names (a
/// `.DS_Store` beside the File Provider roots, the note Dropbox leaves at
/// `~/Dropbox` after moving to File Provider) or a symlink to a root already
/// counted is not one.
fn roots(context: &DetectContext<'_>, detected: &mut Detected) -> Vec<(Provider, PathBuf)> {
    let mut roots = Vec::new();
    let icloud = context.under_home(ICLOUD_ROOT);
    if is_directory(context, &icloud, detected) {
        roots.push((Provider::ICloud, icloud));
    }
    let storage = context.under_home(CLOUD_STORAGE_DIR);
    if is_directory(context, &storage, detected) {
        match context.fs.read_dir(&storage) {
            Ok(entries) => {
                for entry in entries.into_iter().filter(|entry| is_directory(context, entry, detected)) {
                    let name = entry.file_name().and_then(|name| name.to_str()).unwrap_or_default();
                    roots.push((Provider::of_storage_entry(name), entry));
                }
            }
            Err(error) => detected.warnings.push(Detected::unreadable(
                Category::CloudSynced,
                &storage,
                "the File Provider roots",
                &error,
            )),
        }
    }
    for (folder, provider) in LEGACY_ROOTS {
        let root = context.under_home(folder);
        if is_directory(context, &root, detected) {
            roots.push((provider, root));
        }
    }
    roots
}

/// `true` for a real directory: not a file, not a symlink, not a placeholder.
/// One Broza may not even `stat` is a warning; one that is not there is not.
fn is_directory(context: &DetectContext<'_>, path: &Path, detected: &mut Detected) -> bool {
    match context.fs.metadata(path) {
        Ok(meta) => meta.is_dir && !meta.is_symlink && !meta.is_dataless,
        Err(BrozaError::TargetNotFound(_)) => false,
        Err(error) => {
            detected.warnings.push(Detected::unreadable(
                Category::CloudSynced,
                path,
                "its synced files",
                &error,
            ));
            false
        }
    }
}

/// The local bytes and files of one root: a walk that counts placeholders as
/// nothing and never enters a directory the provider has not downloaded.
fn measure(context: &DetectContext<'_>, root: &Path) -> Result<Option<Measured>, BrozaError> {
    let walked = walk(root, &WalkOptions::default(), context.fs);
    let Some(node) = walked.root() else {
        let reason = walked
            .errors
            .first()
            .map_or_else(|| "the root could not be walked".to_owned(), |e| e.message.clone());
        return Err(BrozaError::Other(reason));
    };
    if node.allocated_bytes == 0 {
        return Ok(None);
    }
    let local_files = node.file_count.saturating_sub(node.dataless_count);
    Ok(Some(Measured {
        path: FindingPath {
            path: root.to_path_buf(),
            size_bytes: node.allocated_bytes,
            last_used: node.mtime,
        },
        local_files,
    }))
}

/// The inform-only finding for one provider.
fn finding(provider: Provider, measured: Vec<Measured>) -> Result<crate::model::Finding, BrozaError> {
    let local_files = measured.iter().fold(0_u64, |sum, m| sum.saturating_add(m.local_files));
    let mut paths: Vec<FindingPath> = measured.into_iter().map(|m| m.path).collect();
    paths.sort_by(by_size_then_path);
    let bytes = paths.iter().fold(0_u64, |sum, path| sum.saturating_add(path.size_bytes));
    let title = format!("Already backed up in {}", provider.display_name());
    start(Category::CloudSynced, provider.slug(), &title)?
        .description(format!(
            "Local copies of files {} already holds; the provider can release them on request.",
            provider.display_name()
        ))
        .reasoning(
            "Deleting a synced file locally deletes it from the cloud and from every other device, so \
             Broza only reports these and shows the provider's own steps.",
        )
        .reclaimable_bytes(bytes)
        .item_count(local_files)
        .paths(paths)
        .instructions(provider.instructions())
        .build()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::detect::test_support::{context_over, home};
    use crate::model::{Action, Risk};
    use crate::testing::FakeFileOps;

    const H: &str = "/System/Volumes/Data/Users/dana";

    fn fs() -> FakeFileOps {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        fs.add_dir(H);
        for (path, size) in [
            (format!("{H}/Library/Mobile Documents/com~apple~CloudDocs/Photos/trip.heic"), 2_000_000_000_u64),
            (format!("{H}/Library/Mobile Documents/iCloud~md~obsidian/notes.md"), 4_000_000),
            (format!("{H}/Library/CloudStorage/Dropbox-Personal/work.pdf"), 700_000_000),
            (format!("{H}/Library/CloudStorage/pCloud Drive/x.bin"), 50_000_000),
            (format!("{H}/Google Drive/old-sync/report.docx"), 30_000_000),
        ] {
            fs.add_file(&path, &[]);
            fs.set_size(&path, size);
        }
        // Evicted: on the provider, not on this disk.
        fs.add_dataless_file(
            format!("{H}/Library/Mobile Documents/com~apple~CloudDocs/archive.zip"),
            5_000_000_000,
        );
        fs.add_dataless_dir(format!("{H}/Library/CloudStorage/Dropbox-Personal/Camera Uploads"));
        fs
    }

    fn detect(fs: &FakeFileOps) -> Detected {
        let world = context_over(fs, home());
        CloudSynced.detect(&world.context()).unwrap()
    }

    #[test]
    fn one_red_inform_only_finding_per_provider_measuring_only_what_is_on_disk() {
        let detected = detect(&fs());

        let ids: Vec<String> = detected.findings.iter().map(|f| f.id().to_string()).collect();
        assert_eq!(
            ids,
            vec![
                "cloud-synced.icloud",
                "cloud-synced.dropbox",
                "cloud-synced.google-drive",
                "cloud-synced.other"
            ],
            "{detected:?}"
        );
        let icloud = &detected.findings[0];
        assert_eq!(
            (icloud.risk(), icloud.action(), icloud.is_actionable()),
            (Risk::Red, Action::InformOnly, false)
        );
        let block = |bytes: u64| bytes.div_ceil(4096) * 4096;
        assert_eq!(
            icloud.reclaimable_bytes(),
            block(2_000_000_000) + block(4_000_000),
            "the placeholder is 0 bytes"
        );
        assert_eq!(icloud.item_count(), Some(2), "local files only");
        assert_eq!(icloud.instructions().map(|i| i.provider.as_str()), Some("iCloud Drive"));
        assert!(icloud.instructions().unwrap().steps.iter().any(|s| s.contains("Optimize Mac Storage")));
        let dropbox = &detected.findings[1];
        assert_eq!(dropbox.paths()[0].path, Path::new(&format!("{H}/Library/CloudStorage/Dropbox-Personal")));
        assert!(detected.warnings.is_empty(), "{:?}", detected.warnings);
    }

    #[test]
    fn a_file_or_a_symlink_where_a_root_would_be_is_not_a_root() {
        let fs = fs();
        // Dropbox leaves a note at the old place and a `.DS_Store` beside the roots.
        fs.add_file(
            format!("{H}/Dropbox"),
            b"Your Dropbox folder has moved to ~/Library/CloudStorage/Dropbox",
        );
        fs.add_file(format!("{H}/Library/CloudStorage/.DS_Store"), b"ds");
        fs.add_symlink(format!("{H}/OneDrive"), format!("{H}/Library/CloudStorage/Dropbox-Personal"));

        let detected = detect(&fs);

        let ids: Vec<String> = detected.findings.iter().map(|f| f.id().to_string()).collect();
        assert_eq!(
            ids,
            vec![
                "cloud-synced.icloud",
                "cloud-synced.dropbox",
                "cloud-synced.google-drive",
                "cloud-synced.other"
            ],
            "{detected:?}"
        );
        assert!(detected.warnings.is_empty(), "nothing to warn about: {:?}", detected.warnings);
    }

    #[test]
    fn a_home_without_cloud_roots_reports_nothing() {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        fs.add_file(format!("{H}/Documents/notes.txt"), b"x");

        let detected = detect(&fs);

        assert!(detected.findings.is_empty() && detected.warnings.is_empty(), "{detected:?}");
    }

    #[test]
    fn an_unreadable_cloud_storage_directory_is_a_warning_and_the_rest_still_reports() {
        let fs = fs();
        fs.add_denied(format!("{H}/Library/CloudStorage"));

        let detected = detect(&fs);

        let ids: Vec<String> = detected.findings.iter().map(|f| f.id().to_string()).collect();
        assert_eq!(ids, vec!["cloud-synced.icloud", "cloud-synced.google-drive"]);
        assert_eq!(detected.warnings.len(), 1, "{:?}", detected.warnings);
    }
}
