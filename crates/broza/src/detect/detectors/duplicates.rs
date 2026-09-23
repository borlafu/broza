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
//! so it reads as a duplicate whose removal frees nothing — and neither does
//! removing the original while a clone of it stands. A clone, and a file with
//! a clone among the reported files, are no candidates, like a hard link.

use std::collections::{BTreeMap, HashSet};
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
        let families = families_with_clones(context.home_files);
        let mut by_size: BTreeMap<u64, Vec<&FileEntry>> = BTreeMap::new();
        for file in context.home_files.iter().filter(|file| is_candidate(file, &families)) {
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

/// In the size window, with one name, sharing its blocks with nothing, and not
/// somewhere identical files are meant to be.
fn is_candidate(file: &FileEntry, families: &HashSet<(u64, u64)>) -> bool {
    (DUPLICATE_MIN_BYTES..DUPLICATE_MAX_BYTES).contains(&file.size_bytes)
        && file.link_count <= 1
        && !file.is_clone()
        && !families.contains(&(file.device, file.inode))
        && !file.path.components().any(is_skipped_component)
}

/// The `(device, original inode)` of every clone family with a clone among
/// `files`: an original in one of them still has a clone holding its blocks.
fn families_with_clones(files: &[FileEntry]) -> HashSet<(u64, u64)> {
    files
        .iter()
        .filter(|file| file.is_clone())
        .filter_map(|file| Some((file.device, file.clone_id?)))
        .collect()
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
         others are proposed. APFS clones share their blocks, so removing one frees nothing: neither \
         a clone nor a file that has one is proposed."
    )
}

#[cfg(test)]
#[path = "duplicates_tests.rs"]
mod tests;
