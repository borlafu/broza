//! Benchmark of the walker against the performance budget of `docs/cli-spec.md` §7.
//!
//! **This is the one test that touches the real `$HOME`**, which is why it is
//! `#[ignore]`d: it never runs in CI or in a normal `cargo test`. It only reads —
//! no file is created outside a temporary directory — and it exists because the
//! M2 exit criteria are stated in seconds, not in assertions.
//!
//! ```bash
//! scripts/bench-scan.sh
//! ```
//!
//! `BENCH_ROOT` chooses what to walk (the home directory by default) and
//! `BENCH_EXCLUDE` is a colon-separated list of prefixes to leave out. Those
//! exclusions matter: a home directory holding Dropbox, iCloud Drive or Google
//! Drive is backed by a network filesystem whose `lstat` blocks on the provider,
//! and the benchmark would then measure somebody else's servers, not this walker.
#![cfg(feature = "test-support")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use broza::adapters::{StdFileOps, SystemClock};
use broza::ports::Clock;
use broza::scan::cache::{CacheKey, CacheStore, DirRecord, store_path};
use broza::scan::walker::{DirIdentity, WalkOptions, WalkResult, walk};

/// Cache lifetime used by the benchmark, long enough that nothing expires.
const BENCH_TTL: Duration = Duration::from_secs(60 * 60);
/// Files below this are not collected, mirroring the default `--min-size`.
const BENCH_MIN_FILE_BYTES: u64 = 100_000_000;
/// Volume directory the benchmark writes its store under.
const BENCH_VOLUME: &str = "bench";
/// How many files the benchmark keeps, as `--top` would.
const BENCH_TOP: usize = 20;
/// Levels the benchmark reports, as `--depth` would; the cache may not answer
/// above this, because those levels are what a report shows.
const BENCH_DEPTH: usize = 2;
/// Entries per second a cold walk must manage.
///
/// The budget of `docs/cli-spec.md` §7 is under ten seconds for the boot disk.
/// A Data volume of a few hundred gigabytes runs to a few million entries, so
/// this is the rate that makes that budget reachable — stated as a rate so the
/// assertion means the same thing on a bigger or smaller home directory.
const MIN_ENTRIES_PER_SECOND: f64 = 40_000.0;
/// Separator of the `BENCH_EXCLUDE` list.
const EXCLUDE_SEPARATOR: char = ':';

/// Prefixes the benchmark leaves out, from `BENCH_EXCLUDE`.
fn exclusions() -> Vec<PathBuf> {
    std::env::var("BENCH_EXCLUDE")
        .unwrap_or_default()
        .split(EXCLUDE_SEPARATOR)
        .filter(|prefix| !prefix.trim().is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Walk `root` once, with `store` answering for unchanged subtrees.
fn timed_walk(root: &Path, store: &CacheStore) -> (WalkResult, Duration) {
    let hook = |identity: &DirIdentity| {
        let record = CacheKey::of(identity).and_then(|key| store.lookup(&key))?;
        (record.largest_item_bytes < BENCH_MIN_FILE_BYTES).then(|| record.clone())
    };
    let options = WalkOptions {
        skip_hook: Some(&hook),
        cache_from_depth: BENCH_DEPTH + 1,
        report_files_min_size: Some(BENCH_MIN_FILE_BYTES),
        report_files_top: BENCH_TOP,
        exclude: exclusions(),
        ..WalkOptions::default()
    };
    let started = Instant::now();
    let result = walk(root, &options, &StdFileOps);
    (result, started.elapsed())
}

/// Entries the walk looked at: every directory plus every file it counted.
fn entry_count(result: &WalkResult) -> u64 {
    result.root().map_or(0, |root| root.file_count + root.dir_count + 1)
}

/// Report one run the way the exit criteria are written.
fn report(label: &str, result: &WalkResult, elapsed: Duration) {
    let root = result.root();
    println!(
        "{label}: {:.2} s, {} entries, {} bytes, {} nodes, {} warnings",
        elapsed.as_secs_f64(),
        entry_count(result),
        root.map_or(0, |node| node.size_bytes),
        result.nodes.len(),
        result.errors.len()
    );
}

#[test]
#[ignore = "reads the real $HOME and takes seconds; run it from scripts/bench-scan.sh"]
fn bench_home_walk() {
    let chosen = std::env::var_os("BENCH_ROOT").or_else(|| std::env::var_os("HOME"));
    let Some(home) = chosen.map(PathBuf::from) else {
        println!("skipped: no BENCH_ROOT and no home directory");
        return;
    };
    println!("root: {} (excluding {:?})", home.display(), exclusions());
    let cache = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let store_path = store_path(cache.path(), BENCH_VOLUME);
    let clock = SystemClock;

    let (cold_result, cold) = timed_walk(&home, &CacheStore::empty(&clock, BENCH_TTL));
    report("cold", &cold_result, cold);

    let now = clock.now();
    let records = cold_result.nodes.iter().filter_map(|node| DirRecord::of(node, now));
    let saved = CacheStore::empty(&clock, BENCH_TTL).with_records(records);
    saved.save(&store_path, &StdFileOps).unwrap_or_else(|e| panic!("save: {e}"));
    let store_bytes = std::fs::metadata(&store_path).map_or_else(|e| panic!("stat: {e}"), |meta| meta.len());

    let loaded =
        CacheStore::load(&store_path, &StdFileOps, &clock, BENCH_TTL).unwrap_or_else(|e| panic!("load: {e}"));
    let (warm_result, warm) = timed_walk(&home, &loaded);
    report("warm", &warm_result, warm);
    println!("store: {store_bytes} bytes for {} records", loaded.len());

    assert!(cold_result.root().is_some(), "the walk found nothing at {}", home.display());
    assert_eq!(
        warm_result.root().map(|node| node.size_bytes),
        cold_result.root().map(|node| node.size_bytes),
        "a warm walk must report the same disk as the cold one"
    );
    let rate = entries_per_second(&cold_result, cold);
    assert!(
        rate >= MIN_ENTRIES_PER_SECOND,
        "cold walk managed {rate:.0} entries/s, below the {MIN_ENTRIES_PER_SECOND:.0} the \
         ten-second budget of docs/cli-spec.md §7 needs"
    );
    assert!(warm <= cold, "the cache made the walk slower: {warm:?} against {cold:?}");
}

/// Entries per second a run managed.
#[expect(
    clippy::cast_precision_loss,
    reason = "a rate printed to zero decimals does not need every bit of a u64"
)]
fn entries_per_second(result: &WalkResult, elapsed: Duration) -> f64 {
    entry_count(result) as f64 / elapsed.as_secs_f64().max(f64::EPSILON)
}
