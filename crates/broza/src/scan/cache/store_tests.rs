//! Tests of [`CacheStore`](super::CacheStore): loading, TTL, saving, subtrees.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;

use super::{CacheStore, STORE_FILE_NAME, Verdicts, store_path};
use crate::BrozaError;
use crate::ExitCode;
use crate::ports::{Clock, FileOps};
use crate::scan::cache::key::{CacheKey, DirRecord, KeptClone};
use crate::scan::walker::DirIdentity;
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
        kept_clones: Vec::new(),
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
        child_dirs: Vec::new(),
        files: Vec::new(),
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

/// A record whose child list and file list are given.
fn record_with(
    inode: u64,
    children: &[(&str, u64)],
    files: &[(&str, u64)],
    recorded_at: Timestamp,
) -> DirRecord {
    use crate::scan::cache::key::{ChildDir, FileRecord};
    let child_dirs = children
        .iter()
        .map(|(name, inode)| ChildDir {
            name: name.as_bytes().to_vec(),
            device: 1,
            inode: *inode,
            mtime_ns: Some(42),
        })
        .collect();
    let files = files
        .iter()
        .map(|(name, size)| FileRecord {
            name: name.as_bytes().to_vec(),
            size_bytes: *size,
            allocated_bytes: *size,
            inode: 900 + *size,
            link_count: 1,
            modified_ns: Some(1),
            accessed_ns: Some(2),
            clone_id: None,
        })
        .collect();
    DirRecord { child_dirs, files, ..record(inode, recorded_at) }
}

fn identity_of(inode: u64) -> DirIdentity {
    DirIdentity {
        path: PathBuf::from("/vol/a"),
        device: 1,
        inode,
        mtime: Timestamp::from_nanosecond(42).ok(),
    }
}

#[test]
fn a_served_subtree_names_its_kept_clones_by_path_and_refuses_a_name_that_is_not_one() {
    let clock = FixedClock::default();
    let now = clock.now();
    let kept = |name: &[u8]| KeptClone { family: (1, 77), name: name.to_vec() };
    let store = CacheStore::empty(&clock, TTL).with_records([
        DirRecord { kept_clones: vec![kept(b"keeper.mov")], ..record_with(1, &[("sub", 2)], &[], now) },
        DirRecord { kept_clones: vec![kept(b"deep.mov")], ..record_with(2, &[], &[], now) },
    ]);
    let escaping = CacheStore::empty(&clock, TTL).with_records([DirRecord {
        kept_clones: vec![kept(b"../keeper.mov")],
        ..record_with(1, &[], &[], now)
    }]);

    let served = store.subtree(&identity_of(1), &Verdicts::new()).unwrap_or_else(|| panic!("servable"));

    assert_eq!(
        served.kept_clones,
        vec![(PathBuf::from("/vol/a/keeper.mov"), (1, 77)), (PathBuf::from("/vol/a/sub/deep.mov"), (1, 77))]
    );
    assert!(
        escaping.subtree(&identity_of(1), &Verdicts::new()).is_none(),
        "a keeper name with a slash is refused"
    );
}

#[test]
fn a_subtree_is_rebuilt_whole_with_its_files_or_not_at_all() {
    let clock = FixedClock::default();
    let now = clock.now();
    let whole = CacheStore::empty(&clock, TTL).with_records([
        record_with(1, &[("sub", 2)], &[("big.bin", 5_000_000)], now),
        record_with(2, &[], &[("deep.bin", 2_000_000)], now),
    ]);
    let half = CacheStore::empty(&clock, TTL).with_records([record_with(1, &[("sub", 2)], &[], now)]);

    let served = whole.subtree(&identity_of(1), &Verdicts::new()).unwrap_or_else(|| panic!("servable"));
    let paths: Vec<PathBuf> = served.nodes.iter().map(|node| node.path.clone()).collect();
    let files: Vec<PathBuf> = served.files.iter().map(|file| file.path.clone()).collect();

    assert_eq!(paths, vec![PathBuf::from("/vol/a"), PathBuf::from("/vol/a/sub")]);
    assert_eq!(files, vec![PathBuf::from("/vol/a/big.bin"), PathBuf::from("/vol/a/sub/deep.bin")]);
    assert!(served.nodes.iter().all(|node| node.from_cache));
    assert_eq!(served.files[0].size_bytes, 5_000_000);
    assert!(
        half.subtree(&identity_of(1), &Verdicts::new()).is_none(),
        "a child without a record makes the parent unservable"
    );
}

#[test]
fn a_store_that_loops_or_hides_a_hard_link_is_not_served() {
    let clock = FixedClock::default();
    let now = clock.now();
    let looping = CacheStore::empty(&clock, TTL).with_records([record_with(1, &[("me", 1)], &[], now)]);
    let linked = CacheStore::empty(&clock, TTL)
        .with_records([DirRecord { has_hard_links: true, ..record_with(1, &[], &[], now) }]);

    assert!(looping.subtree(&identity_of(1), &Verdicts::new()).is_none());
    assert!(linked.subtree(&identity_of(1), &Verdicts::new()).is_none());
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

#[test]
fn a_record_naming_a_path_component_that_is_not_plain_is_not_served() {
    let clock = FixedClock::default();
    let now = clock.now();
    for bad in ["", ".", "..", "a/b"] {
        let store =
            CacheStore::empty(&clock, TTL).with_records([record_with(1, &[], &[(bad, 5_000_000)], now)]);
        assert!(store.subtree(&identity_of(1), &Verdicts::new()).is_none(), "{bad:?} must not be served");
    }
    let escaping = CacheStore::empty(&clock, TTL)
        .with_records([record_with(1, &[("..", 2)], &[], now), record_with(2, &[], &[], now)]);
    assert!(escaping.subtree(&identity_of(1), &Verdicts::new()).is_none());
}

#[test]
fn a_refused_subtree_is_remembered_so_its_ancestors_are_not_rebuilt_again() {
    let clock = FixedClock::default();
    let now = clock.now();
    // root(1) -> mid(2) -> leaf(3), and leaf has no record.
    let store = CacheStore::empty(&clock, TTL)
        .with_records([record_with(1, &[("mid", 2)], &[], now), record_with(2, &[("leaf", 3)], &[], now)]);
    let verdicts = Verdicts::new();

    assert!(store.subtree(&identity_of(1), &verdicts).is_none());

    let mid = CacheKey { device: 1, inode: 2, mtime_ns: 42 };
    assert!(verdicts.refused(&CacheKey { device: 1, inode: 1, mtime_ns: 42 }), "the root was refused");
    assert!(verdicts.refused(&mid), "and so was the directory the walker asks about next");
    assert!(verdicts.refused(&CacheKey { device: 1, inode: 3, mtime_ns: 42 }));
}

#[test]
fn a_store_in_which_two_directories_share_a_child_is_not_served_and_does_not_hang() {
    let clock = FixedClock::default();
    let now = clock.now();
    // Each level has two children that are the same record: 2^40 paths for a
    // check that visits each key once.
    let mut records = Vec::new();
    for level in 1..=40_u64 {
        records.push(record_with(level, &[("x", level + 1), ("y", level + 1)], &[], now));
    }
    records.push(record_with(41, &[], &[], now));
    let store = CacheStore::empty(&clock, TTL).with_records(records);

    assert!(store.subtree(&identity_of(1), &Verdicts::new()).is_none());
}

#[test]
fn a_chain_deeper_than_any_real_tree_is_not_served_and_a_shorter_one_below_it_still_is() {
    let clock = FixedClock::default();
    let now = clock.now();
    let depth = u64::try_from(super::MAX_SUBTREE_DEPTH).unwrap_or(u64::MAX) + 10;
    let records = (1..=depth).map(|inode| {
        let children: Vec<(&str, u64)> = if inode < depth { vec![("d", inode + 1)] } else { Vec::new() };
        record_with(inode, &children, &[], now)
    });
    let store = CacheStore::empty(&clock, TTL).with_records(records);
    let verdicts = Verdicts::new();

    assert!(store.subtree(&identity_of(1), &verdicts).is_none(), "too deep from the top");
    let lower = DirIdentity { inode: depth - 5, ..identity_of(1) };
    let served = store.subtree(&lower, &verdicts).unwrap_or_else(|| panic!("servable from lower down"));
    assert_eq!(served.nodes.len(), 6, "a depth refusal is not remembered against the chain");
}

#[test]
fn a_child_on_another_device_is_looked_up_under_its_own_device() {
    use crate::scan::cache::key::ChildDir;
    let clock = FixedClock::default();
    let now = clock.now();
    let child_on_other_device = ChildDir { name: b"mnt".to_vec(), device: 7, inode: 2, mtime_ns: Some(42) };
    let root = DirRecord { child_dirs: vec![child_on_other_device], ..record(1, now) };
    let child = DirRecord { key: CacheKey { device: 7, inode: 2, mtime_ns: 42 }, ..record(2, now) };
    let store = CacheStore::empty(&clock, TTL).with_records([root, child]);

    let served = store.subtree(&identity_of(1), &Verdicts::new()).unwrap_or_else(|| panic!("servable"));

    assert_eq!(served.nodes.len(), 2);
    assert_eq!(served.nodes[1].device, 7);
    assert_eq!(served.nodes[1].path, PathBuf::from("/vol/a/mnt"));
}
