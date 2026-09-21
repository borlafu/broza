//! Throttled scan progress.
//!
//! The CLI must show something within 500 ms (`docs/cli-spec.md` §7) without
//! turning a walk of a million files into a million writes to stderr. The walker
//! records every entry it sees; the reporter forwards a running total to the
//! caller's callback at most once every [`PROGRESS_INTERVAL`].
//!
//! Progress is conversation, never data: the callback the CLI installs writes to
//! stderr (`AGENTS.md` §2.6).

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

use jiff::Timestamp;

use crate::ports::Clock;

/// Shortest gap between two progress callbacks.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Value of `last_emitted` while a thread is inside the callback.
///
/// No real instant can be it, and every other thread reads it as "somebody is
/// already reporting", which is exactly what should keep them quiet.
const CLAIMED: i64 = i64::MIN;
/// Value of `last_emitted` before anything has been reported.
const NEVER: i64 = i64::MIN + 1;

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
/// Shared across the walker's threads and lock-free: the counters are atomic
/// and the right to report is claimed with one compare-and-exchange, so a
/// walker thread never waits on another to finish writing to the terminal.
pub struct ProgressReporter<'a> {
    /// Where a progress update goes.
    sink: &'a (dyn Fn(ScanProgress) + Sync),
    /// Time source, injected so tests do not sleep.
    clock: &'a dyn Clock,
    /// Shortest gap between two emissions, in nanoseconds.
    interval_nanos: i64,
    /// When the last update was emitted, or [`CLAIMED`] while one is going out.
    last_emitted: AtomicI64,
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
            interval_nanos: i64::try_from(interval.as_nanos()).unwrap_or(i64::MAX),
            last_emitted: AtomicI64::new(NEVER),
            entries: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        }
    }

    /// Add `entries` and `bytes` to the total, emitting when the interval elapsed.
    pub fn record(&self, entries: u64, bytes: u64) {
        self.entries.fetch_add(entries, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        let now = nanos_of(self.clock.now());
        if !self.claim(now) {
            return;
        }
        // The claim is held across the callback on purpose: releasing it first
        // would let a second thread report an older total after this one.
        (self.sink)(self.snapshot());
        self.last_emitted.store(now, Ordering::Release);
    }

    /// Emit the running total now, whatever the interval says.
    pub fn flush(&self) {
        let now = nanos_of(self.clock.now());
        self.last_emitted.store(CLAIMED, Ordering::Release);
        (self.sink)(self.snapshot());
        self.last_emitted.store(now, Ordering::Release);
    }

    /// The running total.
    pub fn snapshot(&self) -> ScanProgress {
        ScanProgress {
            entries_scanned: self.entries.load(Ordering::Relaxed),
            bytes_scanned: self.bytes.load(Ordering::Relaxed),
        }
    }

    /// Take the right to report, if it is free and the interval has passed.
    fn claim(&self, now: i64) -> bool {
        let last = self.last_emitted.load(Ordering::Acquire);
        if last == CLAIMED || (last != NEVER && now.saturating_sub(last) < self.interval_nanos) {
            return false;
        }
        self.last_emitted.compare_exchange(last, CLAIMED, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }
}

/// Nanoseconds since the epoch, saturating at the ends of the range.
fn nanos_of(instant: Timestamp) -> i64 {
    i64::try_from(instant.as_nanosecond()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, PoisonError};

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
    fn the_debug_of_a_reporter_shows_the_running_total() {
        let seen = Mutex::new(Vec::new());
        let report = sink(&seen);
        let clock = FixedClock::default();
        let reporter = ProgressReporter::new(&report, &clock);

        reporter.record(2, 20);

        let shown = format!("{reporter:?}");
        assert!(shown.contains("entries_scanned: 2"), "{shown}");
        assert!(shown.contains("bytes_scanned: 20"), "{shown}");
    }
}
