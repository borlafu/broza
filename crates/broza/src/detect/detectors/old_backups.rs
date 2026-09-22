//! `old-backups`: iPhone and iPad backups the Finder keeps (`docs/cli-spec.md` §3.3).
//!
//! One finding, `old-backups.ios-devices`, amber and `quarantine`: every backup
//! under `~/Library/Application Support/MobileSync/Backup/<udid>` whose
//! `Info.plist` says its last backup is older than `--unused-after`. The size is
//! the walk's; the date and the device come from the plist. A backup whose
//! `Info.plist` cannot be read or parsed is a `location_unreadable` warning and
//! is not proposed: a backup Broza cannot date is not one it can call old.

use std::path::Path;

use jiff::Timestamp;
use serde::Deserialize;

use crate::BrozaError;
use crate::model::{Category, FindingPath};

use super::support::{by_size_then_path, finish, path_with, start};
use crate::detect::detector::{DetectContext, Detected, Detector};

/// Where the Finder (and iTunes before it) keeps device backups, under the home.
const BACKUPS_DIR: &str = "Library/Application Support/MobileSync/Backup";
/// The manifest at the top of every backup.
const INFO_PLIST: &str = "Info.plist";

/// The `old-backups` detector.
pub struct OldBackups;

impl Detector for OldBackups {
    fn category(&self) -> Category {
        Category::OldBackups
    }

    fn detect(&self, context: &DetectContext<'_>) -> Result<Detected, BrozaError> {
        let root = context.under_home(BACKUPS_DIR);
        let mut detected = Detected::default();
        if !context.fs.exists(&root) {
            return Ok(detected);
        }
        let backups = match context.fs.read_dir(&root) {
            Ok(children) => children,
            Err(error) => {
                detected.warnings.push(Detected::unreadable(
                    Category::OldBackups,
                    &root,
                    "device backups",
                    &error,
                ));
                return Ok(detected);
            }
        };
        let mut paths: Vec<FindingPath> = Vec::new();
        let mut devices: Vec<String> = Vec::new();
        for backup in backups.iter().filter(|dir| context.node(dir).is_some()) {
            match read_info(context, backup) {
                Ok(info) if info.last_backup.is_some_and(|at| context.is_unused_since(at)) => {
                    devices.push(info.label());
                    paths.push(path_with(backup, context.allocated_bytes(backup), info.last_backup));
                }
                Ok(_) => {}
                Err(error) => detected.warnings.push(Detected::unreadable(
                    Category::OldBackups,
                    backup,
                    "this device backup",
                    &error,
                )),
            }
        }
        paths.sort_by(by_size_then_path);
        devices.sort();
        let builder = start(Category::OldBackups, "ios-devices", "iOS device backups")?
            .description("iPhone and iPad backups the Finder keeps; a newer backup or the device itself supersedes them.")
            .reasoning(format!(
                "Last backed up more than --unused-after ago: {}. A device you still own backs up again on its next sync.",
                devices.join(", ")
            ));
        Ok(detected.with_finding(finish(builder, paths)?))
    }
}

/// What `Info.plist` says about one backup.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Info {
    #[serde(rename = "Device Name")]
    device_name: Option<String>,
    #[serde(rename = "Product Name")]
    product_name: Option<String>,
    #[serde(rename = "Last Backup Date")]
    last_backup_date: Option<plist::Date>,
}

/// The parsed manifest, with the date as a timestamp.
struct BackupInfo {
    device_name: Option<String>,
    product_name: Option<String>,
    last_backup: Option<Timestamp>,
}

impl BackupInfo {
    /// `iPhone de Dana (iPhone 15 Pro)`, or whichever half the plist has.
    fn label(&self) -> String {
        match (&self.device_name, &self.product_name) {
            (Some(device), Some(product)) => format!("{device} ({product})"),
            (Some(device), None) => device.clone(),
            (None, Some(product)) => product.clone(),
            (None, None) => "unnamed device".to_owned(),
        }
    }
}

/// Read and parse `<backup>/Info.plist`.
fn read_info(context: &DetectContext<'_>, backup: &Path) -> Result<BackupInfo, BrozaError> {
    let bytes = context.fs.read(&backup.join(INFO_PLIST))?;
    let info: Info = plist::from_bytes(&bytes).map_err(|error| {
        BrozaError::Other(format!("`{}` is not a backup manifest: {error}", backup.display()))
    })?;
    let last_backup =
        info.last_backup_date.map(std::time::SystemTime::from).and_then(|at| Timestamp::try_from(at).ok());
    Ok(BackupInfo { device_name: info.device_name, product_name: info.product_name, last_backup })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::detect::detector::LOCATION_UNREADABLE_CODE;
    use crate::detect::test_support::{context_over, home};
    use crate::model::{Action, Risk};
    use crate::testing::FakeFileOps;

    const H: &str = "/System/Volumes/Data/Users/dana";

    fn info(device: &str, product: &str, date: &str) -> Vec<u8> {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Device Name</key><string>{device}</string>
<key>Product Name</key><string>{product}</string>
<key>Last Backup Date</key><date>{date}</date>
</dict></plist>"#
        )
        .into_bytes()
    }

    fn fs() -> FakeFileOps {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        let backups = format!("{H}/Library/Application Support/MobileSync/Backup");
        fs.add_file(
            format!("{backups}/00008030-OLD/Info.plist"),
            &info("Dana's iPhone", "iPhone 11", "2023-05-01T10:00:00Z"),
        );
        fs.add_file(format!("{backups}/00008030-OLD/Manifest.db"), &[]);
        fs.set_size(format!("{backups}/00008030-OLD/Manifest.db"), 5_000_000_000);
        fs.add_file(
            format!("{backups}/00008120-NEW/Info.plist"),
            &info("Dana's iPad", "iPad Air", "2026-09-01T10:00:00Z"),
        );
        fs.add_file(format!("{backups}/00008120-NEW/Manifest.db"), &[]);
        fs.set_size(format!("{backups}/00008120-NEW/Manifest.db"), 2_000_000_000);
        fs
    }

    fn detect(fs: &FakeFileOps) -> Detected {
        let world = context_over(fs, home());
        OldBackups.detect(&world.context()).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn a_backup_older_than_the_threshold_is_proposed_and_a_recent_one_is_not() {
        let detected = detect(&fs());

        assert_eq!(detected.findings.len(), 1, "{detected:?}");
        let finding = &detected.findings[0];
        assert_eq!(finding.id().to_string(), "old-backups.ios-devices");
        assert_eq!(finding.paths().len(), 1);
        assert!(finding.paths()[0].path.ends_with("00008030-OLD"));
        assert_eq!(finding.paths()[0].last_used.map(|t| t.to_string()), Some("2023-05-01T10:00:00Z".into()));
        assert!(finding.reclaimable_bytes() >= 5_000_000_000, "{}", finding.reclaimable_bytes());
        assert!(
            finding.reasoning().unwrap().contains("Dana's iPhone (iPhone 11)"),
            "{:?}",
            finding.reasoning()
        );
        assert_eq!(finding.action(), Action::Quarantine);
        assert_eq!(finding.risk(), Risk::Amber);
        assert!(detected.warnings.is_empty(), "{:?}", detected.warnings);
    }

    #[test]
    fn a_backup_without_a_readable_manifest_is_a_warning_and_never_proposed() {
        let fs = fs();
        let broken = format!("{H}/Library/Application Support/MobileSync/Backup/00008999-BAD");
        fs.add_file(format!("{broken}/Info.plist"), b"not a plist");
        fs.add_file(format!("{broken}/Manifest.db"), &[]);

        let detected = detect(&fs);

        assert!(detected.findings[0].paths().iter().all(|p| !p.path.ends_with("00008999-BAD")));
        assert_eq!(detected.warnings.len(), 1, "{:?}", detected.warnings);
        assert_eq!(detected.warnings[0].code, LOCATION_UNREADABLE_CODE);
    }

    #[test]
    fn a_home_without_backups_yields_nothing() {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        fs.add_dir(home());

        let detected = detect(&fs);

        assert!(detected.findings.is_empty() && detected.warnings.is_empty(), "{detected:?}");
    }
}
