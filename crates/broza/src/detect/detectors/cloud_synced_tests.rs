//! Tests of the `cloud-synced` detector, beside it.

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
    fs.add_file(format!("{H}/Dropbox"), b"Your Dropbox folder has moved to ~/Library/CloudStorage/Dropbox");
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
fn every_root_this_detector_claims_is_one_the_home_walk_and_the_guard_keep_out_of() {
    use crate::scan::request::CLOUD_ROOTS;
    for (folder, _) in LEGACY_ROOTS {
        assert!(CLOUD_ROOTS.contains(&folder), "{folder} must be a shared cloud root");
    }
    assert!(CLOUD_ROOTS.contains(&ICLOUD_ROOT) && CLOUD_ROOTS.contains(&CLOUD_STORAGE_DIR));
}

#[test]
fn a_hole_inside_a_cloud_root_is_a_warning_and_the_figure_is_a_floor() {
    let fs = fs();
    fs.add_denied(format!("{H}/Library/Mobile Documents/com~apple~CloudDocs/Photos"));

    let detected = detect(&fs);

    let icloud = detected.findings.iter().find(|f| f.id().to_string() == "cloud-synced.icloud").unwrap();
    assert!(icloud.reclaimable_bytes() < 2_000_000_000, "the unreadable photos are not counted");
    assert!(icloud.reasoning().unwrap().contains("floor"));
    assert_eq!(detected.warnings.len(), 1, "{:?}", detected.warnings);
    assert!(detected.warnings[0].message.contains("part of its synced files"), "{:?}", detected.warnings);
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
