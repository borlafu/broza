//! What the `duplicates` detector must and must not propose.

#![allow(clippy::unwrap_used)]

use jiff::Timestamp;

use super::*;
use crate::detect::test_support::{context_over, home};
use crate::model::{Action, Finding, Risk};
use crate::ports::FileOps;
use crate::testing::FakeFileOps;

const H: &str = "/System/Volumes/Data/Users/dana";
const ORIGINAL: &str = "/System/Volumes/Data/Users/dana/Documents/report.pdf";
const COPY_A: &str = "/System/Volumes/Data/Users/dana/Downloads/report copy.pdf";
const COPY_B: &str = "/System/Volumes/Data/Users/dana/Desktop/report (2).pdf";
const SAME_EDGES: &str = "/System/Volumes/Data/Users/dana/Movies/same-edges.bin";
const SAME_SIZE: &str = "/System/Volumes/Data/Users/dana/Movies/same-size.bin";
const IN_LIBRARY: &str = "/System/Volumes/Data/Users/dana/Library/Application Support/X/report.pdf";
const IN_GIT: &str = "/System/Volumes/Data/Users/dana/code/x/.git/objects/pack/report.pdf";
const IN_BUNDLE: &str = "/System/Volumes/Data/Users/dana/Applications/X.app/Contents/report.pdf";
const IN_PHOTOS: &str =
    "/System/Volumes/Data/Users/dana/Pictures/Photos.photoslibrary/derivatives/report.pdf";
const IN_HIDDEN: &str = "/System/Volumes/Data/Users/dana/.cache/uv/archive/report.pdf";
const IN_TARGET: &str = "/System/Volumes/Data/Users/dana/code/x/target/release/report.pdf";
const IN_PROJECT_LIBRARY: &str = "/System/Volumes/Data/Users/dana/code/game/Library/Bee/report.pdf";
const LINKED: &str = "/System/Volumes/Data/Users/dana/Library/Mail/report-link.pdf";
const CLONED: &str = "/System/Volumes/Data/Users/dana/Documents/report clone.pdf";
const SMALL_A: &str = "/System/Volumes/Data/Users/dana/Documents/a.txt";
const SMALL_B: &str = "/System/Volumes/Data/Users/dana/Documents/b.txt";
const HUGE_A: &str = "/System/Volumes/Data/Users/dana/Movies/huge-a.mov";
const HUGE_B: &str = "/System/Volumes/Data/Users/dana/Movies/huge-b.mov";
const CLOUD: &str = "/System/Volumes/Data/Users/dana/Documents/report-in-the-cloud.pdf";

fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

fn contents(seed: u8) -> Vec<u8> {
    let mut bytes = vec![0_u8; 1_500_000];
    bytes[0] = seed;
    bytes
}

fn fs() -> FakeFileOps {
    let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
    fs.add_dir(H);
    for path in [
        ORIGINAL,
        COPY_A,
        COPY_B,
        IN_LIBRARY,
        IN_GIT,
        IN_BUNDLE,
        IN_PHOTOS,
        IN_HIDDEN,
        IN_TARGET,
        IN_PROJECT_LIBRARY,
    ] {
        fs.add_file(path, &contents(0));
    }
    fs.set_times(ORIGINAL, at("2024-01-01T00:00:00Z"), at("2024-01-01T00:00:00Z"));
    fs.set_times(COPY_A, at("2025-01-01T00:00:00Z"), at("2025-01-01T00:00:00Z"));
    fs.set_times(COPY_B, at("2026-01-01T00:00:00Z"), at("2026-01-01T00:00:00Z"));
    // Same size and same first and last 4 KiB, different in the middle.
    let mut middle_differs = contents(0);
    middle_differs[750_000] = 7;
    fs.add_file(SAME_EDGES, &middle_differs);
    // Same size, different first byte.
    fs.add_file(SAME_SIZE, &contents(1));
    fs.add_file(SMALL_A, b"same");
    fs.add_file(SMALL_B, b"same");
    fs
}

fn detect(fs: &FakeFileOps) -> Detected {
    let world = context_over(fs, home());
    Duplicates.detect(&world.context()).unwrap()
}

fn proposed(detected: &Detected) -> Vec<String> {
    detected.findings.iter().flat_map(Finding::paths).map(|p| p.path.display().to_string()).collect()
}

#[test]
fn every_copy_but_the_oldest_is_proposed_and_lookalikes_are_not() {
    let detected = detect(&fs());

    assert_eq!(proposed(&detected), vec![COPY_B.to_owned(), COPY_A.to_owned()], "{detected:?}");
    let finding = &detected.findings[0];
    assert_eq!(finding.id().to_string(), "duplicates.home");
    assert_eq!((finding.risk(), finding.action()), (Risk::Amber, Action::Quarantine));
    assert_eq!(finding.item_count(), Some(2));
    assert_eq!(
        finding.reclaimable_bytes(),
        2 * 1_500_000_u64.div_ceil(4096) * 4096,
        "allocated, not apparent"
    );
    assert_eq!(finding.paths()[0].last_used, Some(at("2026-01-01T00:00:00Z")), "the walk's access time");
    assert!(finding.reasoning().unwrap().starts_with("1 group(s)"), "{:?}", finding.reasoning());
    assert!(detected.warnings.is_empty(), "{:?}", detected.warnings);
}

#[test]
fn a_file_with_a_second_name_anywhere_is_no_candidate() {
    let fs = fs();
    fs.add_hard_link(ORIGINAL, LINKED);

    let detected = detect(&fs);

    // The original dropped out, so the next oldest copy is the one kept.
    assert_eq!(proposed(&detected), vec![COPY_B.to_owned()], "{detected:?}");
}

#[test]
fn a_clone_and_the_file_it_shares_blocks_with_are_no_candidates() {
    let fs = fs();
    fs.add_clone(ORIGINAL, CLONED);
    fs.set_times(CLONED, at("2020-01-01T00:00:00Z"), at("2020-01-01T00:00:00Z"));

    let detected = detect(&fs);

    // The clone would have been the oldest copy, and the original still has a
    // clone holding its blocks: both drop out, and the two real copies remain.
    assert_eq!(proposed(&detected), vec![COPY_B.to_owned()], "{detected:?}");
    assert!(
        detected.findings[0].reasoning().unwrap().contains("clone"),
        "{:?}",
        detected.findings[0].reasoning()
    );
}

#[test]
fn files_of_a_gigabyte_or_more_are_left_to_large_old_files() {
    let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
    for path in [HUGE_A, HUGE_B] {
        fs.add_file(path, &[]);
        fs.set_size(path, DUPLICATE_MAX_BYTES);
    }

    let detected = detect(&fs);

    assert!(detected.findings.is_empty(), "{detected:?}");
}

#[test]
fn a_cloud_placeholder_is_never_read() {
    let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
    fs.add_file(ORIGINAL, &contents(0));
    fs.add_dataless_file(CLOUD, 1_500_000);

    let detected = detect(&fs);

    assert!(detected.findings.is_empty() && detected.warnings.is_empty(), "{detected:?}");
}

#[test]
fn among_copies_modified_at_the_same_time_the_first_path_is_kept() {
    let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
    for path in [COPY_A, COPY_B] {
        fs.add_file(path, &contents(0));
        fs.set_times(path, at("2025-01-01T00:00:00Z"), at("2025-01-01T00:00:00Z"));
    }

    let detected = detect(&fs);

    // `Desktop` sorts before `Downloads`, so the desktop copy is kept.
    assert_eq!(proposed(&detected), vec![COPY_A.to_owned()], "{detected:?}");
}

#[test]
fn a_home_with_nothing_alike_reports_nothing() {
    let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
    fs.add_file(SMALL_A, b"same");
    fs.add_file(SMALL_B, b"same");
    fs.add_file(SAME_SIZE, &contents(1));
    fs.add_file(ORIGINAL, &contents(0));

    let detected = detect(&fs);

    assert!(detected.findings.is_empty() && detected.warnings.is_empty(), "{detected:?}");
}

#[test]
fn a_file_that_vanishes_before_comparison_is_a_warning_and_the_group_stands() {
    let fs = fs();
    let world = context_over(&fs, home());
    // Walked, then gone: the context still lists it.
    fs.remove_tree(Path::new(COPY_A)).unwrap();

    let detected = Duplicates.detect(&world.context()).unwrap();

    assert_eq!(proposed(&detected), vec![COPY_B.to_owned()], "{detected:?}");
    assert_eq!(detected.warnings.len(), 1, "{:?}", detected.warnings);
    assert_eq!(detected.warnings[0].path.as_deref(), Some(Path::new(COPY_A)));
}
