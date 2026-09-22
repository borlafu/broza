//! `duplicates`: identical copies of the same file (`docs/cli-spec.md` §3.3).
//!
//! One finding, `duplicates.home`, amber and `quarantine`. The candidates are
//! the files of at least [`DUPLICATE_MIN_BYTES`] the home walk reported,
//! outside `~/Library` and `~/.Trash`, outside `.app` bundles and outside
//! `.git` and `node_modules` trees, where identical files are the software's
//! own. Comparison is staged so that almost nothing is read: by size, then by
//! the first 4 KiB (`PREFIX_BYTES`), then by a full BLAKE3 hash, and only a group
//! that survives all three is a group of duplicates. Two names of one inode
//! are one file. In each group the copy with the oldest modification time is
//! kept and the rest are proposed.
//!
//! An APFS clone holds the same bytes as its original and shares its blocks,
//! so it reads as a duplicate whose removal frees nothing; the reasoning says
//! so until clone accounting lands.

use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};

use crate::BrozaError;
use crate::model::{Category, FindingPath};
use crate::ports::{ContentHash, EntryMetadata};
use crate::scan::FileEntry;

use super::support::{by_size_then_path, finish, path_with, start};
use crate::detect::detector::{DetectContext, Detected, Detector};

/// Smaller files are not worth a read: the walk reports from here up.
pub const DUPLICATE_MIN_BYTES: u64 = 1_000_000;
/// How much of a file the second stage compares.
const PREFIX_BYTES: usize = 4096;
/// Directories under the home whose files belong to other detectors.
const SKIPPED_UNDER_HOME: [&str; 2] = ["Library", ".Trash"];
/// Path components that mark a tree whose identical files are deliberate.
const SKIPPED_COMPONENTS: [&str; 2] = [".git", "node_modules"];
/// An application bundle: its files are the app's, however many are identical.
const BUNDLE_SUFFIX: &str = ".app";

/// The `duplicates` detector.
pub struct Duplicates;

impl Detector for Duplicates {
    fn category(&self) -> Category {
        Category::Duplicates
    }

    fn detect(&self, context: &DetectContext<'_>) -> Result<Detected, BrozaError> {
        let skipped: Vec<PathBuf> = SKIPPED_UNDER_HOME.iter().map(|dir| context.under_home(dir)).collect();
        let mut by_size: BTreeMap<u64, Vec<&FileEntry>> = BTreeMap::new();
        for file in context.home_files.iter().filter(|file| is_candidate(file, &skipped)) {
            by_size.entry(file.size_bytes).or_default().push(file);
        }
        let mut detected = Detected::default();
        let mut groups = 0_usize;
        let mut paths: Vec<FindingPath> = Vec::new();
        for same_size in by_size.into_values().filter(|group| group.len() >= 2) {
            let confirmed = confirm(&same_size, context, &mut detected);
            groups += confirmed.len();
            paths.extend(confirmed.into_iter().flat_map(propose));
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

/// Big enough, and not somewhere identical files are meant to be.
fn is_candidate(file: &FileEntry, skipped: &[PathBuf]) -> bool {
    file.size_bytes >= DUPLICATE_MIN_BYTES
        && !skipped.iter().any(|dir| file.path.starts_with(dir))
        && !file.path.components().any(is_skipped_component)
}

fn is_skipped_component(component: Component<'_>) -> bool {
    let Component::Normal(name) = component else {
        return false;
    };
    let Some(name) = name.to_str() else {
        return false;
    };
    SKIPPED_COMPONENTS.contains(&name) || name.ends_with(BUNDLE_SUFFIX)
}

/// One file under comparison: what the walk said and what `lstat` says.
struct Candidate<'a> {
    entry: &'a FileEntry,
    meta: EntryMetadata,
}

/// The groups of identical files among files of one size: first by the start
/// of the file, then by the hash of all of it. A file that cannot be read is
/// a warning and drops out of its group.
fn confirm<'a>(
    same_size: &[&'a FileEntry],
    context: &DetectContext<'_>,
    detected: &mut Detected,
) -> Vec<Vec<Candidate<'a>>> {
    let distinct = distinct_files(same_size, context, detected);
    if distinct.len() < 2 {
        return Vec::new();
    }
    let mut by_prefix: BTreeMap<Vec<u8>, Vec<Candidate<'a>>> = BTreeMap::new();
    for candidate in distinct {
        match context.fs.read_prefix(&candidate.entry.path, PREFIX_BYTES) {
            Ok(prefix) => by_prefix.entry(prefix).or_default().push(candidate),
            Err(error) => warn(detected, &candidate.entry.path, &error),
        }
    }
    let mut by_hash: BTreeMap<ContentHash, Vec<Candidate<'a>>> = BTreeMap::new();
    for candidate in by_prefix.into_values().filter(|group| group.len() >= 2).flatten() {
        match context.fs.hash_file(&candidate.entry.path) {
            Ok(hash) => by_hash.entry(hash).or_default().push(candidate),
            Err(error) => warn(detected, &candidate.entry.path, &error),
        }
    }
    by_hash.into_values().filter(|group| group.len() >= 2).collect()
}

/// The plain files of the group, one per inode: a hard link is a second name,
/// not a second copy.
fn distinct_files<'a>(
    same_size: &[&'a FileEntry],
    context: &DetectContext<'_>,
    detected: &mut Detected,
) -> Vec<Candidate<'a>> {
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let mut distinct = Vec::new();
    for entry in same_size {
        let meta = match context.fs.metadata(&entry.path) {
            Ok(meta) => meta,
            Err(error) => {
                warn(detected, &entry.path, &error);
                continue;
            }
        };
        if meta.is_dir || meta.is_symlink || meta.is_dataless || !seen.insert((meta.device, meta.inode)) {
            continue;
        }
        distinct.push(Candidate { entry, meta });
    }
    distinct
}

/// Every copy but the oldest, as finding paths.
fn propose(group: Vec<Candidate<'_>>) -> Vec<FindingPath> {
    let mut copies = group;
    copies
        .sort_by(|a, b| a.meta.modified.cmp(&b.meta.modified).then_with(|| a.entry.path.cmp(&b.entry.path)));
    copies
        .into_iter()
        .skip(1)
        .map(|copy| path_with(&copy.entry.path, copy.entry.allocated_bytes, copy.meta.accessed))
        .collect()
}

fn warn(detected: &mut Detected, path: &Path, error: &BrozaError) {
    detected.warnings.push(Detected::unreadable(Category::Duplicates, path, "its copies", error));
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
    const SAME_START: &str = "/System/Volumes/Data/Users/dana/Movies/same-start.bin";
    const SAME_SIZE: &str = "/System/Volumes/Data/Users/dana/Movies/same-size.bin";
    const IN_LIBRARY: &str = "/System/Volumes/Data/Users/dana/Library/Application Support/X/report.pdf";
    const IN_GIT: &str = "/System/Volumes/Data/Users/dana/code/x/.git/objects/pack/report.pdf";
    const IN_BUNDLE: &str = "/System/Volumes/Data/Users/dana/Applications/X.app/Contents/report.pdf";
    const LINKED: &str = "/System/Volumes/Data/Users/dana/Documents/report-link.pdf";
    const SMALL_A: &str = "/System/Volumes/Data/Users/dana/Documents/a.txt";
    const SMALL_B: &str = "/System/Volumes/Data/Users/dana/Documents/b.txt";

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
        for path in [ORIGINAL, COPY_A, COPY_B, IN_LIBRARY, IN_GIT, IN_BUNDLE] {
            fs.add_file(path, &contents(0));
        }
        fs.set_times(ORIGINAL, at("2024-01-01T00:00:00Z"), at("2024-01-01T00:00:00Z"));
        fs.set_times(COPY_A, at("2025-01-01T00:00:00Z"), at("2025-01-01T00:00:00Z"));
        fs.set_times(COPY_B, at("2026-01-01T00:00:00Z"), at("2026-01-01T00:00:00Z"));
        // Same size and same first 4 KiB, different further in.
        let mut tail_differs = contents(0);
        tail_differs[1_000_000] = 7;
        fs.add_file(SAME_START, &tail_differs);
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
        assert!(finding.reasoning().unwrap().starts_with("1 group(s)"), "{:?}", finding.reasoning());
        assert!(detected.warnings.is_empty(), "{:?}", detected.warnings);
    }

    #[test]
    fn a_second_name_of_the_same_file_is_not_a_copy() {
        let fs = fs();
        fs.add_hard_link(ORIGINAL, LINKED);

        let detected = detect(&fs);

        assert!(!proposed(&detected).iter().any(|p| p == LINKED), "{detected:?}");
        assert_eq!(proposed(&detected).len(), 2);
    }

    #[test]
    fn two_files_of_one_size_that_differ_are_read_no_further_than_needed() {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        fs.add_file(SAME_SIZE, &contents(1));
        fs.add_file(ORIGINAL, &contents(0));

        let detected = detect(&fs);

        assert!(detected.findings.is_empty(), "{detected:?}");
    }

    #[test]
    fn a_home_with_nothing_alike_reports_nothing() {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        fs.add_file(SMALL_A, b"same");
        fs.add_file(SMALL_B, b"same");

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
