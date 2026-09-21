//! The per-volume store: load, look up under the TTL, extend, save.
//!
//! Immutable: [`CacheStore::with_record`] returns a new store, so the walk that
//! reads from a store and the aggregation that adds to it never fight over one
//! mutable map.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};

use crate::BrozaError;
use crate::ports::{Clock, FileOps};
use crate::scan::cache::codec::{NO_CACHE_HINT, decode, encode};
use crate::scan::cache::key::{CacheKey, DirRecord};

/// Name of the store file inside a volume's cache directory.
pub const STORE_FILE_NAME: &str = "dirs.bin";
/// Directory naming the on-disk layout version (`docs/cli-spec.md` §7).
const LAYOUT_DIR: &str = "v1";

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
        let records = decode(&bytes)?;
        // Anything already expired has to go the next time this is written.
        let expired = records.iter().any(|record| !empty.is_fresh(record));
        let records = records.into_iter().map(|record| (record.key, record)).collect();
        Ok(Self { records, changed: expired, ..empty })
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

    /// A copy of the store with `record` added, replacing any record of its key.
    ///
    /// A record that measures exactly what the stored one measures changes
    /// nothing, and is dropped rather than kept: see
    /// [`CacheStore::has_changes`] and [`CacheStore::save`].
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

    /// `true` when this store holds something the file on disk does not.
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
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use jiff::Timestamp;

    use super::{CacheStore, STORE_FILE_NAME, store_path};
    use crate::BrozaError;
    use crate::ExitCode;
    use crate::ports::{Clock, FileOps};
    use crate::scan::cache::key::{CacheKey, DirRecord};
    use crate::testing::{FakeFileOps, FixedClock};

    /// A day, the default `cache-ttl`.
    const TTL: Duration = Duration::from_secs(24 * 60 * 60);
    /// How many records the TTL-boundary probe writes.
    const RECORDS_IN_THE_PROBE: u64 = 13;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap_or_else(|e| panic!("{e}"))
    }

    fn key(inode: u64) -> CacheKey {
        CacheKey { device: 1, inode, mtime_ns: 42 }
    }

    fn record(inode: u64, recorded_at: Timestamp) -> DirRecord {
        DirRecord {
            key: key(inode),
            size_bytes: 1000 + inode,
            allocated_bytes: 2000,
            file_count: 1,
            dir_count: 0,
            dataless_count: 0,
            largest_item_bytes: 4096,
            has_hard_links: false,
            has_truncation: false,
            recorded_at,
        }
    }

    fn fs() -> FakeFileOps {
        FakeFileOps::new().with_root("/cache", 1)
    }

    fn path() -> PathBuf {
        PathBuf::from("/cache/v1/disk3s5/dirs.bin")
    }

    /// How many records the file holds, as a `u64` the probe can compare.
    fn stored_count(fs: &FakeFileOps, clock: &FixedClock) -> u64 {
        u64::try_from(load(fs, clock).len()).unwrap_or_default()
    }

    fn load(fs: &FakeFileOps, clock: &FixedClock) -> CacheStore {
        CacheStore::load(&path(), fs, clock, TTL).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn a_store_that_was_never_written_is_empty() {
        let store = load(&fs(), &FixedClock::default());

        assert!(store.is_empty());
        assert_eq!(store.lookup(&key(1)), None);
    }

    #[test]
    fn what_was_saved_is_what_is_loaded() {
        let fs = fs();
        let clock = FixedClock::default();
        let saved = load(&fs, &clock).with_record(record(1, clock.now())).with_record(record(2, clock.now()));

        saved.save(&path(), &fs).unwrap_or_else(|e| panic!("{e}"));
        let loaded = load(&fs, &clock);

        assert_eq!(loaded, saved);
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn saving_creates_the_directory_of_the_store() {
        let fs = fs();
        let clock = FixedClock::default();

        load(&fs, &clock)
            .with_record(record(1, clock.now()))
            .save(&path(), &fs)
            .unwrap_or_else(|e| panic!("{e}"));

        assert!(fs.exists(&path()));
    }

    #[test]
    fn the_records_survive_the_ttl_boundary_when_the_disk_has_not_changed() {
        let fs = fs();
        let clock = FixedClock::at(at("2026-01-01T00:00:00Z"));
        let first = (1..=RECORDS_IN_THE_PROBE)
            .fold(load(&fs, &clock), |store, inode| store.with_record(record(inode, clock.now())));
        first.save(&path(), &fs).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(stored_count(&fs, &clock), RECORDS_IN_THE_PROBE);

        // A day and a minute later every record has expired. The scan walks
        // the same unchanged directories and measures the same numbers: the
        // store must take those fresh measurements, not keep the stale ones it
        // is about to drop, or the whole cache empties at every TTL boundary.
        clock.set(at("2026-01-02T00:01:00Z"));
        let renewed = (1..=RECORDS_IN_THE_PROBE)
            .fold(load(&fs, &clock), |store, inode| store.with_record(record(inode, clock.now())));
        renewed.save(&path(), &fs).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(stored_count(&fs, &clock), RECORDS_IN_THE_PROBE, "the cache emptied itself");
        assert_eq!(load(&fs, &clock).lookup(&key(1)).map(|record| record.size_bytes), Some(1001));
    }

    #[test]
    fn a_store_nothing_happened_to_is_not_written_again() {
        let fs = fs();
        let clock = FixedClock::default();
        let saved = load(&fs, &clock).with_record(record(1, clock.now()));
        saved.save(&path(), &fs).unwrap_or_else(|e| panic!("{e}"));
        let written = fs.read(&path()).unwrap_or_else(|e| panic!("{e}"));

        // A second scan measures the same directory again: same numbers, a
        // later instant. Rewriting tens of megabytes for that is the slowest
        // part of a warm scan and buys nothing.
        clock.advance(Duration::from_secs(60));
        let reloaded = load(&fs, &clock).with_record(record(1, clock.now()));

        assert!(!reloaded.has_changes(), "nothing worth writing happened");
        reloaded.save(&path(), &fs).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(fs.read(&path()).unwrap_or_else(|e| panic!("{e}")), written, "the file is untouched");
    }

    #[test]
    fn a_directory_that_really_changed_is_written() {
        let fs = fs();
        let clock = FixedClock::default();
        load(&fs, &clock)
            .with_record(record(1, clock.now()))
            .save(&path(), &fs)
            .unwrap_or_else(|e| panic!("{e}"));

        let grown = DirRecord { size_bytes: 9999, ..record(1, clock.now()) };
        let reloaded = load(&fs, &clock).with_record(grown);

        assert!(reloaded.has_changes());
        reloaded.save(&path(), &fs).unwrap_or_else(|e| panic!("{e}"));
        let after = load(&fs, &clock);
        assert_eq!(after.lookup(&key(1)).map(|record| record.size_bytes), Some(9999));
    }

    #[test]
    fn a_store_holding_something_expired_is_written_even_if_nothing_else_happened() {
        let fs = fs();
        let clock = FixedClock::at(at("2026-01-01T00:00:00Z"));
        load(&fs, &clock)
            .with_record(record(1, clock.now()))
            .save(&path(), &fs)
            .unwrap_or_else(|e| panic!("{e}"));

        clock.set(at("2026-01-03T00:00:00Z"));
        let stale = load(&fs, &clock);

        assert!(stale.has_changes(), "the expired record has to be dropped from the file");
        stale.save(&path(), &fs).unwrap_or_else(|e| panic!("{e}"));
        assert!(load(&fs, &clock).is_empty());
    }

    #[test]
    fn saving_keeps_the_fresh_records_and_forgets_the_expired_ones() {
        let fs = fs();
        let clock = FixedClock::at(at("2026-01-02T00:00:00Z"));
        let store = CacheStore::load(&path(), &fs, &clock, TTL)
            .unwrap_or_else(|e| panic!("{e}"))
            .with_record(record(1, at("2026-01-01T12:00:00Z")))
            .with_record(record(2, at("2025-11-01T00:00:00Z")));

        store.save(&path(), &fs).unwrap_or_else(|e| panic!("{e}"));
        let loaded = CacheStore::load(&path(), &fs, &clock, TTL).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(loaded.len(), 1, "a store that never forgot would grow for ever");
        assert_eq!(loaded.lookup(&key(1)).map(|record| record.size_bytes), Some(1001));
        assert_eq!(loaded.lookup(&key(2)), None);
    }

    #[test]
    fn a_record_inside_the_ttl_is_served_and_an_older_one_is_not() {
        let clock = FixedClock::at(at("2026-01-02T00:00:00Z"));
        let store = CacheStore::load(&path(), &fs(), &clock, TTL)
            .unwrap_or_else(|e| panic!("{e}"))
            .with_record(record(1, at("2026-01-01T12:00:00Z")))
            .with_record(record(2, at("2025-12-30T00:00:00Z")));

        assert_eq!(store.lookup(&key(1)).map(|record| record.size_bytes), Some(1001));
        assert_eq!(store.lookup(&key(2)), None);
    }

    #[test]
    fn a_record_newer_than_the_store_is_fresh_not_stale() {
        // On a real clock every record of this very scan is stamped after the
        // store was opened; a store that dropped them would never fill.
        let clock = FixedClock::at(at("2026-01-02T00:00:00Z"));
        let store = CacheStore::load(&path(), &fs(), &clock, TTL)
            .unwrap_or_else(|e| panic!("{e}"))
            .with_record(record(1, at("2026-01-02T00:00:07Z")));

        assert!(store.lookup(&key(1)).is_some());
        assert_eq!(store.sorted_records().len(), 1, "and it is written back");
    }

    #[test]
    fn a_record_from_far_in_the_future_is_not_trusted() {
        let clock = FixedClock::at(at("2026-01-02T00:00:00Z"));
        let store = CacheStore::load(&path(), &fs(), &clock, TTL)
            .unwrap_or_else(|e| panic!("{e}"))
            .with_record(record(1, at("2027-06-01T00:00:00Z")));

        assert_eq!(store.lookup(&key(1)), None, "a clock that went wrong is not a fresh record");
        assert!(store.sorted_records().is_empty(), "and it is not written back");
    }

    #[test]
    fn adding_a_record_leaves_the_store_it_came_from_alone() {
        let clock = FixedClock::default();
        let original = load(&fs(), &clock);

        let extended = original.clone().with_record(record(1, clock.now()));

        assert!(original.is_empty());
        assert_eq!(extended.len(), 1);
    }

    #[test]
    fn the_newest_record_for_a_key_wins() {
        let clock = FixedClock::at(at("2026-01-02T00:00:00Z"));
        let first = DirRecord { size_bytes: 1, ..record(1, clock.now()) };
        let second = DirRecord { size_bytes: 2, ..record(1, clock.now()) };

        let store = CacheStore::load(&path(), &fs(), &clock, TTL)
            .unwrap_or_else(|e| panic!("{e}"))
            .with_record(first)
            .with_record(second);

        assert_eq!(store.lookup(&key(1)).map(|record| record.size_bytes), Some(2));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn a_corrupt_store_is_reported_as_a_cache_error_and_never_repaired() {
        let fs = fs();
        fs.add_file(path(), b"BRZC\x09garbage");

        let error = CacheStore::load(&path(), &fs, &FixedClock::default(), TTL).err();

        let Some(error @ BrozaError::Cache(_)) = error else { panic!("{error:?}") };
        assert_eq!(ExitCode::from(&error), ExitCode::CacheError);
        assert!(error.to_string().contains("--no-cache"), "{error}");
        assert_eq!(fs.read(&path()).unwrap_or_else(|e| panic!("{e}")), b"BRZC\x09garbage");
    }

    #[test]
    fn a_store_that_cannot_be_read_at_all_is_a_cache_error_too() {
        let fs = fs();
        // Something is at the store's path, but it is not a file Broza can read.
        fs.add_dir(path());

        let error = CacheStore::load(&path(), &fs, &FixedClock::default(), TTL).err();

        assert!(matches!(error, Some(BrozaError::Cache(_))), "{error:?}");
    }

    #[test]
    fn the_store_of_a_volume_lives_under_the_layout_version() {
        let path = store_path(Path::new("/Users/dana/.cache/broza"), "disk3s5");

        assert_eq!(path, PathBuf::from("/Users/dana/.cache/broza/v1/disk3s5").join(STORE_FILE_NAME));
    }
}
