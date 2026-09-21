//! Throttled scan progress.
//!
//! The CLI must show something within 500 ms (`docs/cli-spec.md` §7) without
//! turning a walk of a million files into a million writes to stderr. The walker
//! records every entry it sees; the reporter forwards a running total to the
//! caller's callback at most once every [`PROGRESS_INTERVAL`].
//!
//! Progress is conversation, never data: the callback the CLI installs writes to
//! stderr (`AGENTS.md` §2.6).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};

use crate::ports::Clock;

/// Shortest gap between two progress callbacks.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// How far a scan has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScanProgress {
    /// Filesystem entries looked at so far, directories included.
    pub entries_scanned: u64,
    /// Apparent bytes counted so far.
    pub bytes_scanned: u64,
}

/// Accumulates scan progress and forwards it to a callback, throttled.
///
/// Shared across the walker's threads: the counters are atomic and the last
/// emission instant is behind its own lock, so recording never blocks a walker
/// thread for longer than that lock.
pub struct ProgressReporter<'a> {
    /// Where a progress update goes.
    sink: &'a (dyn Fn(ScanProgress) + Sync),
    /// Time source, injected so tests do not sleep.
    clock: &'a dyn Clock,
    /// Shortest gap between two emissions.
    interval: SignedDuration,
    /// When the last update was emitted; `None` before the first one.
    last_emitted: Mutex<Option<Timestamp>>,
    /// Entries seen so far.
    entries: AtomicU64,
    /// Apparent bytes seen so far.
    bytes: AtomicU64,
}

impl std::fmt::Debug for ProgressReporter<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgressReporter").field("progress", &self.snapshot()).finish_non_exhaustive()
    }
}

impl<'a> ProgressReporter<'a> {
    /// A reporter that emits at most once every [`PROGRESS_INTERVAL`].
    pub fn new(sink: &'a (dyn Fn(ScanProgress) + Sync), clock: &'a dyn Clock) -> Self {
        Self::every(sink, clock, PROGRESS_INTERVAL)
    }

    /// A reporter with an explicit throttling interval.
    pub fn every(sink: &'a (dyn Fn(ScanProgress) + Sync), clock: &'a dyn Clock, interval: Duration) -> Self {
        Self {
            sink,
            clock,
            interval: SignedDuration::try_from(interval).unwrap_or(SignedDuration::MAX),
            last_emitted: Mutex::new(None),
            entries: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        }
    }

    /// Add `entries` and `bytes` to the total, emitting when the interval elapsed.
    pub fn record(&self, entries: u64, bytes: u64) {
        self.entries.fetch_add(entries, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        if self.claim_emission() {
            (self.sink)(self.snapshot());
        }
    }

    /// Emit the running total now, whatever the interval says.
    pub fn flush(&self) {
        *self.guarded_last() = Some(self.clock.now());
        (self.sink)(self.snapshot());
    }

    /// The running total.
    pub fn snapshot(&self) -> ScanProgress {
        ScanProgress {
            entries_scanned: self.entries.load(Ordering::Relaxed),
            bytes_scanned: self.bytes.load(Ordering::Relaxed),
        }
    }

    /// `true` when this caller is the one allowed to emit now.
    fn claim_emission(&self) -> bool {
        let now = self.clock.now();
        let mut last = self.guarded_last();
        if last.is_some_and(|previous| now.duration_since(previous) < self.interval) {
            return false;
        }
        *last = Some(now);
        true
    }

    /// The last emission instant, with a poisoned lock treated as usable.
    fn guarded_last(&self) -> std::sync::MutexGuard<'_, Option<Timestamp>> {
        self.last_emitted.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, PoisonError};
    use std::time::Duration;

    use super::{PROGRESS_INTERVAL, ProgressReporter, ScanProgress};
    use crate::testing::FixedClock;

    fn recorded(seen: &Mutex<Vec<ScanProgress>>) -> Vec<ScanProgress> {
        seen.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn sink(seen: &Mutex<Vec<ScanProgress>>) -> impl Fn(ScanProgress) + Sync {
        move |progress| seen.lock().unwrap_or_else(PoisonError::into_inner).push(progress)
    }

    #[test]
    fn the_first_update_is_reported_immediately() {
        let seen = Mutex::new(Vec::new());
        let report = sink(&seen);
        let clock = FixedClock::default();
        let reporter = ProgressReporter::new(&report, &clock);

        reporter.record(3, 100);

        assert_eq!(recorded(&seen), vec![ScanProgress { entries_scanned: 3, bytes_scanned: 100 }]);
    }

    #[test]
    fn updates_inside_the_interval_are_swallowed_and_later_ones_carry_the_total() {
        let seen = Mutex::new(Vec::new());
        let report = sink(&seen);
        let clock = FixedClock::default();
        let reporter = ProgressReporter::new(&report, &clock);

        reporter.record(1, 10);
        reporter.record(1, 10);
        clock.advance(PROGRESS_INTERVAL);
        reporter.record(1, 10);

        assert_eq!(
            recorded(&seen),
            vec![
                ScanProgress { entries_scanned: 1, bytes_scanned: 10 },
                ScanProgress { entries_scanned: 3, bytes_scanned: 30 },
            ]
        );
    }

    #[test]
    fn a_flush_reports_the_total_even_inside_the_interval() {
        let seen = Mutex::new(Vec::new());
        let report = sink(&seen);
        let clock = FixedClock::default();
        let reporter = ProgressReporter::new(&report, &clock);

        reporter.record(1, 10);
        reporter.record(4, 40);
        reporter.flush();

        assert_eq!(reporter.snapshot(), ScanProgress { entries_scanned: 5, bytes_scanned: 50 });
        assert_eq!(recorded(&seen).len(), 2);
    }

    #[test]
    fn the_default_interval_is_a_tenth_of_a_second() {
        assert_eq!(PROGRESS_INTERVAL, Duration::from_millis(100));
    }
}
