//! Counting an APFS clone family once, and always in the same place.
//!
//! `clonefile(2)` gives a file a second inode over the same blocks; `cp -c`,
//! the Finder's Duplicate and a good many apps use it, and `st_blocks` then
//! reports the shared blocks under every inode of the family. Summed, a
//! folder of cloned media measures more than the disk holds. APFS marks each
//! family with a clone id (`ATTR_CMNEXT_CLONEID`): the original's inode, which
//! the original itself reports too, so an ordinary file and the original of a
//! family read alike (`docs/cli-spec.md` §4.2, PRD RF-02, ADR 0009).
//!
//! The rule, once the walk is done and hard links are settled
//! ([`super::dedupe`]):
//!
//! - when the family's original was seen by the walk, it keeps the bytes and
//!   every clone is discounted — it is the file whose deletion frees nothing
//!   while the original stands;
//! - when it was not (deleted, excluded, on a subtree served from the cache),
//!   the clone whose path sorts first keeps them and the rest are discounted,
//!   as the names of a hard link are.
//!
//! A discounted clone stays in the lists of files with `allocated_bytes` set to
//! zero: removing it frees nothing, and the detectors need to see it to know
//! that the file it was cloned from is spoken for as well (`duplicates`).
//!
//! # Why a ledger and not a sighting per clone
//!
//! A disk with a cloned media folder has a million clones or more, and a record
//! per clone with its path costs a gigabyte before anything is settled. What
//! the rule needs is far less: per family, the first path and what it counted;
//! per directory, what its clones counted; per family, the directories holding
//! one. That is what the walk keeps ([`CloneLedger`]), and the whole pass is
//! linear in the families and the directories.
//!
//! Clones diverge: a clone written to after the fact shares only part of its
//! blocks. Discounting it whole then under-counts by the part it owns. That is
//! the best effort the clone id allows, and it is what the spec promises. A
//! clone that also has several names is settled as a hard link only.
//!
//! # Warm walks
//!
//! A directory holding clones stays cacheable — marking it otherwise, as a
//! hard link's directories are, would re-walk a cloned media folder on every
//! warm run. Instead each cache record names the families whose credited
//! clone lives directly in it ([`Settlement::kept_clones`]), and a subtree
//! served from the cache hands those families back as if their originals had
//! been seen: any other clone the walk meets is discounted, as the cold walk
//! discounted it. What the records cannot say is where an *original* lives,
//! so a family whose original sits in a cached subtree while a clone is walked
//! counts once per side. That errs towards more space in use than there is,
//! and is bounded by `cache-ttl`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ports::EntryMetadata;
use crate::scan::walker::dedupe::{Charge, apply_charges};
use crate::scan::walker::parts::LinkSighting;
use crate::scan::walker::{DirNode, FileEntry};

/// A clone family: `(device, inode of the original)`.
type FamilyKey = (u64, u64);

/// Every file the walk saw that could be the original of a clone family.
///
/// A file whose clone id is its own inode is either an ordinary file or an
/// original, and the walk cannot tell which until it has seen the clones —
/// so it remembers them all, by inode under their device (a walk rarely
/// leaves one), sorted once at the end: eight bytes a file.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Originals(BTreeMap<u64, Vec<u64>>);

impl Originals {
    /// Remember `inode` on `device`.
    pub fn record(&mut self, device: u64, inode: u64) {
        self.0.entry(device).or_default().push(inode);
    }

    /// Both sets together.
    pub fn merge(mut self, other: Self) -> Self {
        for (device, mut inodes) in other.0 {
            self.0.entry(device).or_default().append(&mut inodes);
        }
        self
    }

    /// The same set, ready to be asked ([`Originals::contains`]).
    fn sorted(mut self) -> Self {
        for inodes in self.0.values_mut() {
            inodes.sort_unstable();
            inodes.dedup();
            inodes.shrink_to_fit();
        }
        self
    }

    /// `true` when the walk saw the file. Only after [`Originals::sorted`].
    fn contains(&self, family: FamilyKey) -> bool {
        self.0.get(&family.0).is_some_and(|inodes| inodes.binary_search(&family.1).is_ok())
    }
}

/// What the walk keeps about the clones it meets.
#[derive(Debug, Default)]
pub(super) struct CloneLedger {
    /// Each family seen, by key.
    families: HashMap<FamilyKey, Family>,
    /// What the clones directly inside each directory counted, one entry per
    /// directory that holds any. A directory's clones are all recorded by the
    /// one listing of that directory, so two ledgers never name the same one
    /// and merging is appending; the charges are keyed only when settled.
    charges: Vec<(Arc<Path>, Charge)>,
}

/// One clone family as the walk saw it: the clone whose path sorts first, with
/// what it counted.
#[derive(Debug)]
struct Family {
    first: First,
}

/// The clone of a family that keeps the bytes when the original is gone.
#[derive(Debug, Clone)]
struct First {
    path: PathBuf,
    device: u64,
    inode: u64,
    size_bytes: u64,
    allocated_bytes: u64,
}

impl CloneLedger {
    /// Record a clone at `path`, directly inside `dir`.
    pub fn record(&mut self, dir: &Arc<Path>, path: PathBuf, meta: &EntryMetadata) {
        let Some(clone_id) = meta.clone_id else { return };
        match self.charges.last_mut() {
            Some((charged, charge)) if Arc::ptr_eq(charged, dir) => {
                *charge = charge.plus(&Charge::of_leaf(meta));
            }
            _ => self.charges.push((Arc::clone(dir), Charge::of_leaf(meta))),
        }
        let first = First {
            path,
            device: meta.device,
            inode: meta.inode,
            size_bytes: meta.size_bytes,
            allocated_bytes: meta.allocated_bytes,
        };
        match self.families.entry((meta.device, clone_id)) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(Family { first });
            }
            std::collections::hash_map::Entry::Occupied(mut slot) => {
                let family = slot.get_mut();
                if first.path < family.first.path {
                    family.first = first;
                }
            }
        }
    }

    /// Both ledgers together.
    pub fn merge(self, other: Self) -> Self {
        // The smaller set of families moves into the larger one.
        let (mut kept, other) =
            if other.families.len() > self.families.len() { (other, self) } else { (self, other) };
        for (key, family) in other.families {
            match kept.families.entry(key) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(family);
                }
                std::collections::hash_map::Entry::Occupied(mut slot) => {
                    let mine = slot.get_mut();
                    if family.first.path < mine.first.path {
                        mine.first = family.first;
                    }
                }
            }
        }
        let mut charges = other.charges;
        kept.charges.append(&mut charges);
        kept
    }

    /// `true` when nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.families.is_empty()
    }
}

/// What settling the clones leaves behind.
pub(super) struct Settlement {
    /// The nodes, with clones discounted.
    pub nodes: Vec<DirNode>,
    /// The reported files, discounted clones freeing nothing.
    pub files: Vec<FileEntry>,
    /// The files for the cache, likewise.
    pub cache_files: Vec<FileEntry>,
    /// The clones that keep their family's bytes: direct files of the
    /// directories they are credited to.
    pub credited: Vec<LinkSighting>,
    /// The same, as the cache records them: the credited clone's directory and
    /// its family.
    pub kept_clones: Vec<(PathBuf, FamilyKey)>,
    /// Every family the walk met a clone of.
    pub families: Vec<FamilyKey>,
}

/// Settle every clone family the walk recorded.
pub(super) fn settle_clones(
    nodes: Vec<DirNode>,
    files: Vec<FileEntry>,
    cache_files: Vec<FileEntry>,
    ledger: CloneLedger,
    originals: Originals,
) -> Settlement {
    if ledger.is_empty() {
        return Settlement {
            nodes,
            files,
            cache_files,
            credited: Vec::new(),
            kept_clones: Vec::new(),
            families: Vec::new(),
        };
    }
    let originals = originals.sorted();
    let CloneLedger { families, charges: charged_dirs } = ledger;
    let mut charges: HashMap<&Path, Charge> = HashMap::new();
    for (dir, charge) in &charged_dirs {
        let total = charges.entry(dir.as_ref()).or_default();
        *total = total.plus(charge);
    }
    let mut keepers: HashMap<FamilyKey, PathBuf> = HashMap::new();
    let mut credited = Vec::new();
    let mut kept_clones = Vec::new();
    let mut families: Vec<(FamilyKey, Family)> = families.into_iter().collect();
    families.sort_by_key(|family| family.0);
    let family_keys: Vec<FamilyKey> = families.iter().map(|(key, _)| *key).collect();
    for (key, family) in families {
        if originals.contains(key) {
            continue;
        }
        // The first path keeps the bytes: take its charge back.
        if let Some(dir) = family.first.path.parent()
            && let Some(charge) = charges.get_mut(dir)
        {
            *charge = charge.minus(&Charge::of_first(&family.first));
        }
        credited.push(sighting_of(&family.first));
        if let Some(dir) = family.first.path.parent() {
            kept_clones.push((dir.to_path_buf(), key));
        }
        keepers.insert(key, family.first.path);
    }
    let nodes = apply_charges(nodes, &charges);
    let files = freeing_nothing(files, &keepers);
    let cache_files = freeing_nothing(cache_files, &keepers);
    Settlement { nodes, files, cache_files, credited, kept_clones, families: family_keys }
}

/// Every family the walk knows of: those it met clones of, those subtrees
/// served from the cache keep, and those of the clones in the file lists.
/// Sorted, without repeats.
pub(super) fn families_of(
    met: Vec<FamilyKey>,
    served: Vec<FamilyKey>,
    files: &[FileEntry],
    cache_files: &[FileEntry],
) -> Vec<FamilyKey> {
    let mut families = met;
    families.extend(served);
    let listed = files.iter().chain(cache_files).filter(|file| file.is_clone());
    families.extend(listed.filter_map(|file| Some((file.device, file.clone_id?))));
    families.sort_unstable();
    families.dedup();
    families
}

/// The same files, every discounted clone with no allocated bytes: what
/// removing it frees. A clone is discounted unless it is its family's keeper.
fn freeing_nothing(files: Vec<FileEntry>, keepers: &HashMap<FamilyKey, PathBuf>) -> Vec<FileEntry> {
    files
        .into_iter()
        .map(|file| {
            let family = file.clone_id.map(|id| (file.device, id));
            let discounted = file.is_clone()
                && family.is_some_and(|key| keepers.get(&key).is_none_or(|keeper| *keeper != file.path));
            if discounted { FileEntry { allocated_bytes: 0, ..file } } else { file }
        })
        .collect()
}

/// The keeper of a family, as the sighting the largest-item pass wants.
fn sighting_of(first: &First) -> LinkSighting {
    LinkSighting {
        device: first.device,
        inode: first.inode,
        path: first.path.clone(),
        link_count: 1,
        size_bytes: first.size_bytes,
        allocated_bytes: first.allocated_bytes,
    }
}

impl Charge {
    fn of_leaf(meta: &EntryMetadata) -> Self {
        Self { size_bytes: meta.size_bytes, allocated_bytes: meta.allocated_bytes, files: 1 }
    }

    fn of_first(first: &First) -> Self {
        Self { size_bytes: first.size_bytes, allocated_bytes: first.allocated_bytes, files: 1 }
    }
}

#[cfg(test)]
#[path = "clones_tests.rs"]
mod tests;
