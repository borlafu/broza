//! `duplicates`: identical copies of the same file (`docs/cli-spec.md` §3.3).
//!
//! One finding, `duplicates.home`, amber and `quarantine`. The candidates are
//! the files the home walk reported that are at least [`DUPLICATE_MIN_BYTES`]
//! and under [`DUPLICATE_MAX_BYTES`] (a file that big is `large-old-files`'
//! business, and hashing it would cost seconds), in the user's own trees only:
//! nothing under a hidden directory (`.cache`, `.git`, `.vscode`, `.Trash`), a
//! `Library` (the home's or a project's), a `node_modules`, `target`, `build`
//! or `__pycache__` tree, or inside a macOS package (`.app`, `.photoslibrary`,
//! `.sparsebundle`, …). Identical files there are the software's structure,
//! not copies, and removing one breaks it; they are also where a developer's
//! home keeps most of its duplicates, so leaving them out is what makes the
//! comparison affordable.
//!
//! Comparison is staged so that almost nothing is read: by size, then by the
//! first and last 4 KiB (`EDGE_BYTES`), then by a full BLAKE3 hash, and only a
//! group that survives all three is a group of duplicates. A file with more
//! than one name is not a candidate: quarantining one hard link frees nothing.
//! In each group the copy with the oldest modification time is kept and the
//! rest are proposed. Every date and size comes from the walk, taken before
//! any file was read.
//!
//! An APFS clone holds the same bytes as its original and shares its blocks,
//! so it reads as a duplicate whose removal frees nothing; the reasoning says
//! so until clone accounting lands.

use std::collections::BTreeMap;
use std::path::{Component, Path};

use crate::BrozaError;
use crate::model::{Category, Diagnostic, FindingPath};
use crate::ports::{ContentHash, FileOps};
use crate::scan::{DETECTOR_FILES_MIN_BYTES, FileEntry};

use super::large_old_files::LARGE_FILE_MIN_BYTES;
use super::support::{by_size_then_path, finish, path_with, start};
use crate::detect::detector::{DetectContext, Detected, Detector};

/// Smaller files are not worth a read: the walk reports from here up.
pub const DUPLICATE_MIN_BYTES: u64 = DETECTOR_FILES_MIN_BYTES;
/// From here up a file belongs to `large-old-files`; exclusive.
pub const DUPLICATE_MAX_BYTES: u64 = LARGE_FILE_MIN_BYTES;
/// How much of each end of a file the second stage compares.
const EDGE_BYTES: usize = 4096;
/// Path components that mark a tree whose identical files are deliberate:
/// application support, dependency stores and build outputs.
const SKIPPED_COMPONENTS: [&str; 5] = ["Library", "node_modules", "target", "build", "__pycache__"];
/// A hidden directory or file: a tool's own store, never the user's documents.
const HIDDEN_PREFIX: &str = ".";
/// macOS packages: directories the Finder shows as one app or document. The
/// identical files inside are the package's structure (sparse-bundle bands,
/// photo derivatives, a VM's disks), and the package is broken without them.
const PACKAGE_SUFFIXES: [&str; 15] = [
    ".app",
    ".sparsebundle",
    ".photoslibrary",
    ".fcpbundle",
    ".imovielibrary",
    ".tvlibrary",
    ".musiclibrary",
    ".pvm",
    ".utm",
    ".xcarchive",
    ".band",
    ".rtfd",
    ".pages",
    ".numbers",
    ".key",
];

/// The `duplicates` detector.
pub struct Duplicates;

impl Detector for Duplicates {
    fn category(&self) -> Category {
        Category::Duplicates
    }

    fn detect(&self, context: &DetectContext<'_>) -> Result<Detected, BrozaError> {
        let mut by_size: BTreeMap<u64, Vec<&FileEntry>> = BTreeMap::new();
        for file in context.home_files.iter().filter(|file| is_candidate(file)) {
            by_size.entry(file.size_bytes).or_default().push(file);
        }
        // One size group at a time: the detectors already run in parallel with
        // each other, and after the scope above the comparison costs seconds at
        // most on a developer's home (`docs/implementation-plan.md`, M4).
        let outcomes: Vec<Confirmed<'_>> = by_size
            .into_values()
            .filter(|group| group.len() >= 2)
            .map(|group| confirm(&group, context.fs))
            .collect();
        let mut detected = Detected::default();
        let mut groups = 0_usize;
        let mut paths: Vec<FindingPath> = Vec::new();
        for outcome in outcomes {
            detected.warnings.extend(outcome.warnings);
            groups += outcome.groups.len();
            paths.extend(outcome.groups.into_iter().flat_map(propose));
        }
        paths.sort_by(by_size_then_path);
        let builder = start(Category::Duplicates, "home", "Duplicate files")?
            .description(
                "Identical copies of the same file under your home; the oldest copy of each is kept.",
            )
            .reasoning(reasoning(groups));
        Ok(detected.with_finding(finish(builder, paths)?))
    }
}

/// In the size window, with one name, and not somewhere identical files are
/// meant to be.
fn is_candidate(file: &FileEntry) -> bool {
    (DUPLICATE_MIN_BYTES..DUPLICATE_MAX_BYTES).contains(&file.size_bytes)
        && file.link_count <= 1
        && !file.path.components().any(is_skipped_component)
}

fn is_skipped_component(component: Component<'_>) -> bool {
    let Component::Normal(name) = component else {
        return false;
    };
    let Some(name) = name.to_str() else {
        return false;
    };
    name.starts_with(HIDDEN_PREFIX)
        || SKIPPED_COMPONENTS.contains(&name)
        || PACKAGE_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

/// What comparing the files of one size produced.
struct Confirmed<'a> {
    /// Groups of identical files, each of at least two.
    groups: Vec<Vec<&'a FileEntry>>,
    /// Files that could not be read for comparison.
    warnings: Vec<Diagnostic>,
}

/// The groups of identical files among files of one size: first by both ends
/// of the file, then by the hash of all of it. A file that cannot be read is
/// a warning and drops out of its group.
fn confirm<'a>(same_size: &[&'a FileEntry], fs: &dyn FileOps) -> Confirmed<'a> {
    let mut warnings = Vec::new();
    let mut by_edges: BTreeMap<Vec<u8>, Vec<&'a FileEntry>> = BTreeMap::new();
    for file in same_size {
        match read_edges(fs, file) {
            Ok(edges) => by_edges.entry(edges).or_default().push(file),
            Err(error) => warnings.push(unreadable(&file.path, &error)),
        }
    }
    let mut by_hash: BTreeMap<ContentHash, Vec<&'a FileEntry>> = BTreeMap::new();
    for file in by_edges.into_values().filter(|group| group.len() >= 2).flatten() {
        match fs.hash_file(&file.path) {
            Ok(hash) => by_hash.entry(hash).or_default().push(file),
            Err(error) => warnings.push(unreadable(&file.path, &error)),
        }
    }
    Confirmed { groups: by_hash.into_values().filter(|group| group.len() >= 2).collect(), warnings }
}

/// The first and the last [`EDGE_BYTES`] of the file, back to back. Two files
/// that agree here and in size are read in full; most that differ, differ here.
fn read_edges(fs: &dyn FileOps, file: &FileEntry) -> Result<Vec<u8>, BrozaError> {
    let mut edges = fs.read_range(&file.path, 0, EDGE_BYTES)?;
    let tail_start = file.size_bytes.saturating_sub(EDGE_BYTES as u64);
    if tail_start > 0 {
        edges.extend(fs.read_range(&file.path, tail_start, EDGE_BYTES)?);
    }
    Ok(edges)
}

/// Every copy but the one modified first. Ties go by path; a copy whose
/// modification time is unknown is kept only when no other is.
fn propose(group: Vec<&FileEntry>) -> Vec<FindingPath> {
    let mut copies = group;
    copies.sort_by(|a, b| {
        a.modified
            .is_none()
            .cmp(&b.modified.is_none())
            .then(a.modified.cmp(&b.modified))
            .then(a.path.cmp(&b.path))
    });
    copies
        .into_iter()
        .skip(1)
        .map(|copy| path_with(&copy.path, copy.allocated_bytes, copy.accessed))
        .collect()
}

fn unreadable(path: &Path, error: &BrozaError) -> Diagnostic {
    Detected::unreadable(Category::Duplicates, path, "its copies", error)
}

fn reasoning(groups: usize) -> String {
    format!(
        "{groups} group(s) of files with identical contents; the copy modified first is kept and the \
         others are proposed. An APFS clone reads as a duplicate but shares its blocks with the \
         original, so removing it frees nothing."
    )
}

#[cfg(test)]
mod tests {
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
}
