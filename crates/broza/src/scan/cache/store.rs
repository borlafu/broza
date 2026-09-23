//! The per-volume store: load, look up under the TTL, extend, save.
//!
//! Immutable: [`CacheStore::with_record`] returns a new store, so the walk that
//! reads from a store and the aggregation that adds to it never fight over one
//! mutable map.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};

use crate::BrozaError;
use crate::ports::{Clock, FileOps};
use crate::scan::cache::codec::{NO_CACHE_HINT, decode, encode};
use crate::scan::cache::key::{CacheKey, DirRecord};
use crate::scan::cache::records::{child_key, child_path, file_of, is_plain_name, node_of};
use crate::scan::walker::{CachedSubtree, DirIdentity};

/// Name of the store file inside a volume's cache directory.
pub const STORE_FILE_NAME: &str = "dirs.bin";
/// Directory the stores live under. Fixed: the layout version travels in the
/// file's own header byte ([`crate::scan::cache::STORE_VERSION`]), not here.
const LAYOUT_DIR: &str = "v1";
/// Deepest subtree the store rebuilds; a chain longer than any real tree is a
/// store that lies, and is walked instead.
const MAX_SUBTREE_DEPTH: usize = 512;

/// What one walk has already learnt about the directories it asked about.
///
/// Shared by every lookup of the walk: a subtree that passed is not validated
/// again when the walker asks about it directly, and one that failed refuses
/// every ancestor without a second descent. Interior mutability because the
/// walker's hook is a shared `Fn`. Read only to answer faster, never to serve
/// what a fresh check would refuse.
#[derive(Debug, Default)]
pub struct Verdicts(Mutex<HashMap<CacheKey, bool>>);

impl Verdicts {
    /// Nothing decided yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&self, key: &CacheKey) -> Option<bool> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).get(key).copied()
    }

    fn record(&self, key: CacheKey, servable: bool) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).insert(key, servable);
    }

    /// `true` when the walk already refused `key`.
    #[cfg(test)]
    fn refused(&self, key: &CacheKey) -> bool {
        self.get(key) == Some(false)
    }
}

/// What checking one record concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    /// The record and everything below it can be served.
    Servable,
    /// Something below is missing, stale, unusable or misnamed: a property of
    /// the record itself, remembered for the walk.
    Refused,
    /// The check ran out of depth or met a key twice: a property of where the
    /// check started, not of the record, so nothing is remembered.
    Bounded,
}

/// Warning code for a cache filed under a BSD name for want of a UUID.
pub const BSD_ID_KEY_CODE: &str = "cache_keyed_by_bsd_id";

/// Where the store of `volume` lives under `cache_root`.
///
/// `cache_root` is `~/.cache/broza` in practice; the core never reads `$HOME`
/// itself, so the CLI passes it in (`AGENTS.md` §4). The directory is named
/// after the volume's UUID when there is one: `disk4s1` is whichever disk is
/// plugged into that slot today, while the UUID follows the volume
/// (`docs/cli-spec.md` §7).
pub fn store_path(cache_root: &Path, volume: &str) -> PathBuf {
    cache_root.join(LAYOUT_DIR).join(volume).join(STORE_FILE_NAME)
}

/// The cached directory aggregates of one volume.
#[derive(Debug, Clone, Eq)]
pub struct CacheStore {
    /// Records by key.
    records: HashMap<CacheKey, DirRecord>,
    /// When the store was opened; every TTL check is against this instant.
    opened_at: Timestamp,
    /// How long a record stays usable.
    ttl: SignedDuration,
    /// Whether anything worth writing has happened since it was loaded.
    ///
    /// A warm scan re-measures directories and gets the same numbers back; the
    /// only difference is the instant of the measurement, and rewriting
    /// fifteen megabytes to record that would cost more than the walk it just
    /// saved. So a record that measures the same is left alone, timestamp and
    /// all — which also means a reused measurement expires on the schedule the
    /// TTL promises rather than being renewed for ever.
    changed: bool,
}

/// Two stores are the same when they hold the same records under the same
/// clock. Whether one of them still has to be written is not part of that.
impl PartialEq for CacheStore {
    fn eq(&self, other: &Self) -> bool {
        self.records == other.records && self.opened_at == other.opened_at && self.ttl == other.ttl
    }
}

impl CacheStore {
    /// Load the store at `path`, or an empty one when there is nothing there.
    ///
    /// A missing file is the normal first run. Anything else that stops Broza
    /// from reading the file — a decode failure, a wrong magic, a version from
    /// the future, or an unreadable file — is a [`BrozaError::Cache`], never a
    /// silent repair (`docs/cli-spec.md` §7).
    pub fn load(path: &Path, fs: &dyn FileOps, clock: &dyn Clock, ttl: Duration) -> Result<Self, BrozaError> {
        let empty = Self::empty(clock, ttl);
        if !fs.exists(path) {
            return Ok(empty);
        }
        let bytes = match fs.read(path) {
            Ok(bytes) => bytes,
            // The file vanished between the check and the read: no cache, no drama.
            Err(BrozaError::TargetNotFound(_)) => return Ok(empty),
            Err(error) => return Err(BrozaError::Cache(format!("{error}; {NO_CACHE_HINT}"))),
        };
        let decoded = decode(&bytes)?;
        // Anything already expired has to go the next time this is written,
        // and so does a file in an older layout.
        let expired = decoded.records.iter().any(|record| !empty.is_fresh(record));
        let records = decoded.records.into_iter().map(|record| (record.key, record)).collect();
        Ok(Self { records, changed: expired || decoded.outdated, ..empty })
    }

    /// An empty store that answers against `clock` and `ttl`.
    pub fn empty(clock: &dyn Clock, ttl: Duration) -> Self {
        Self {
            records: HashMap::new(),
            opened_at: clock.now(),
            ttl: SignedDuration::try_from(ttl).unwrap_or(SignedDuration::MAX),
            changed: false,
        }
    }

    /// The record for `key`, when there is one and it is still fresh.
    ///
    /// A record stamped more than one TTL in the future is treated as expired: the
    /// clock moved, and a scan that trusts it would report sizes nobody can
    /// reproduce. This scan's own records, stamped moments after the store was
    /// opened on a ticking clock, are fresh.
    pub fn lookup(&self, key: &CacheKey) -> Option<&DirRecord> {
        self.records.get(key).filter(|record| self.is_fresh(record))
    }

    /// The whole subtree under `identity`, rebuilt from the store, or nothing.
    ///
    /// Nothing when the directory has no fresh usable record, or when any
    /// directory below it has none: a subtree is served whole or not at all,
    /// so a warm scan never reports a directory whose contents it half knows.
    /// The check runs first over keys alone, remembering each verdict in
    /// `verdicts` for the rest of the walk; only a subtree that passed is
    /// materialised. The directory asked about comes first, then its
    /// descendants, each with the big files the record kept.
    pub fn subtree(&self, identity: &DirIdentity, verdicts: &Verdicts) -> Option<CachedSubtree> {
        let key = CacheKey::of(identity)?;
        let mut seen = HashSet::new();
        if self.check(key, &mut Vec::new(), &mut seen, verdicts) != Check::Servable {
            return None;
        }
        let record = self.lookup(&key)?;
        let mut subtree = CachedSubtree::default();
        self.rebuild(&identity.path, record, &mut subtree);
        Some(subtree)
    }

    /// Whether `key` and everything below it can be served: fresh, usable,
    /// plainly named, each key met once, and no deeper than
    /// [`MAX_SUBTREE_DEPTH`]. A store in which two directories share a child
    /// (`seen`) or a chain runs deeper than any real tree describes no
    /// filesystem, and is walked instead.
    fn check(
        &self,
        key: CacheKey,
        stack: &mut Vec<CacheKey>,
        seen: &mut HashSet<CacheKey>,
        verdicts: &Verdicts,
    ) -> Check {
        if let Some(servable) = verdicts.get(&key) {
            return if servable && seen.insert(key) { Check::Servable } else { Check::Refused };
        }
        if stack.len() >= MAX_SUBTREE_DEPTH || !seen.insert(key) {
            return Check::Bounded;
        }
        let Some(record) = self.lookup(&key).filter(|record| record.is_usable()) else {
            verdicts.record(key, false);
            return Check::Refused;
        };
        let names_are_plain = record.child_dirs.iter().map(|child| child.name.as_slice()).all(is_plain_name)
            && record.files.iter().map(|file| file.name.as_slice()).all(is_plain_name);
        if !names_are_plain {
            verdicts.record(key, false);
            return Check::Refused;
        }
        stack.push(key);
        let mut verdict = Check::Servable;
        for child in &record.child_dirs {
            verdict = match child_key(child) {
                Some(child_key) => self.check(child_key, stack, seen, verdicts),
                None => Check::Refused,
            };
            if verdict != Check::Servable {
                break;
            }
        }
        stack.pop();
        match verdict {
            Check::Servable => verdicts.record(key, true),
            Check::Refused => verdicts.record(key, false),
            Check::Bounded => {}
        }
        verdict
    }

    /// Add `record` at `path` and everything below it, after [`Self::check`]
    /// said yes. The store is immutable and both passes run over `&self`, so
    /// nothing can change between them; a key that still fails to resolve
    /// simply ends its branch.
    fn rebuild(&self, path: &Path, record: &DirRecord, subtree: &mut CachedSubtree) {
        subtree.nodes.push(node_of(path, record));
        subtree.files.extend(record.files.iter().map(|file| file_of(path, record.key.device, file)));
        subtree.kept_clones.extend(record.kept_clones.iter().copied());
        for child in &record.child_dirs {
            if let Some(child_record) = child_key(child).and_then(|key| self.lookup(&key)) {
                self.rebuild(&child_path(path, child), child_record, subtree);
            }
        }
    }

    /// The same store with `record` added, replacing an older record of the
    /// same key unless that one is fresh and measures the same.
    #[must_use]
    pub fn with_record(self, record: DirRecord) -> Self {
        // Only a record that is *still fresh* may be kept: an expired one is
        // about to be dropped on the next write, and keeping it in preference
        // to the measurement that just replaced it would empty the cache at
        // every TTL boundary.
        let keep_the_old_one = self
            .records
            .get(&record.key)
            .is_some_and(|stored| stored.measures_the_same_as(&record) && self.is_fresh(stored));
        if keep_the_old_one {
            return self;
        }
        let mut records = self.records;
        records.insert(record.key, record);
        Self { records, changed: true, ..self }
    }

    /// `true` when saving would write something the file does not already say:
    /// a new or changed record, an expired one to drop, or an older layout.
    pub fn has_changes(&self) -> bool {
        self.changed
    }

    /// A copy of the store with every record of `records` added.
    #[must_use]
    pub fn with_records(self, records: impl IntoIterator<Item = DirRecord>) -> Self {
        records.into_iter().fold(self, Self::with_record)
    }

    /// Write the store to `path`, creating its directory when needed.
    pub fn save(&self, path: &Path, fs: &dyn FileOps) -> Result<(), BrozaError> {
        if let Some(parent) = path.parent() {
            fs.create_dir_all(parent)?;
        }
        fs.write_atomic(path, &encode(&self.sorted_records())?)
    }

    /// How many records the store holds, fresh or not.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// `true` when the store holds no record at all.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// When this store was opened: the instant this scan's records are stamped with.
    pub fn opened_at(&self) -> Timestamp {
        self.opened_at
    }

    /// `true` when `record` is within one TTL of the store's opening, either way.
    fn is_fresh(&self, record: &DirRecord) -> bool {
        // A record measured *after* the store was opened — this very scan, on a
        // real clock — is as fresh as they come, so a small negative age is not
        // stale. A record from far in the future is a clock that went wrong,
        // and it is not trusted beyond the same TTL in that direction.
        let age = self.opened_at.duration_since(record.recorded_at);
        age <= self.ttl && age >= -self.ttl
    }

    /// Every record still worth keeping, in a stable order.
    ///
    /// Borrowed, not cloned: a large volume has hundreds of thousands of them,
    /// and they are about to be serialised once and dropped.
    ///
    /// Expired records are left out rather than lived with: a store that only
    /// ever grew would keep every directory the disk has ever had.
    fn sorted_records(&self) -> Vec<&DirRecord> {
        let mut records: Vec<&DirRecord> =
            self.records.values().filter(|record| self.is_fresh(record)).collect();
        records.sort_by_key(|record| (record.key.device, record.key.inode, record.key.mtime_ns));
        records
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
