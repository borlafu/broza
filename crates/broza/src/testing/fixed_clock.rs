//! Frozen [`Clock`].

use std::sync::Mutex;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};

use crate::ports::Clock;
use crate::testing::sync::lock;

/// Instant a [`FixedClock::default`] starts at.
const DEFAULT_NOW: &str = "2026-01-01T00:00:00Z";

/// A clock that never moves unless a test moves it.
#[derive(Debug)]
pub struct FixedClock {
    /// Instant returned by [`Clock::now`].
    now: Mutex<Timestamp>,
}

impl FixedClock {
    /// A clock frozen at `now`.
    pub fn at(now: Timestamp) -> Self {
        Self { now: Mutex::new(now) }
    }

    /// Move the clock forward, saturating at [`Timestamp::MAX`].
    pub fn advance(&self, duration: Duration) {
        let mut now = lock(&self.now);
        let step = SignedDuration::try_from(duration).unwrap_or(SignedDuration::MAX);
        *now = now.checked_add(step).unwrap_or(Timestamp::MAX);
    }

    /// Set the clock to `now`.
    pub fn set(&self, now: Timestamp) {
        *lock(&self.now) = now;
    }
}

impl Default for FixedClock {
    fn default() -> Self {
        Self::at(DEFAULT_NOW.parse::<Timestamp>().unwrap_or(Timestamp::UNIX_EPOCH))
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        *lock(&self.now)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use jiff::Timestamp;

    use super::{DEFAULT_NOW, FixedClock};
    use crate::ports::Clock;

    fn at(text: &str) -> Timestamp {
        text.parse::<Timestamp>().unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn a_frozen_clock_returns_the_same_instant_twice() {
        let clock = FixedClock::at(at("2026-09-21T10:00:00Z"));

        assert_eq!(clock.now(), clock.now());
        assert_eq!(clock.now(), at("2026-09-21T10:00:00Z"));
    }

    #[test]
    fn advancing_moves_the_clock_forward_by_the_duration() {
        let clock = FixedClock::at(at("2026-09-21T10:00:00Z"));

        clock.advance(Duration::from_secs(90));

        assert_eq!(clock.now(), at("2026-09-21T10:01:30Z"));
    }

    #[test]
    fn advancing_past_the_end_of_time_saturates() {
        let clock = FixedClock::at(Timestamp::MAX);

        clock.advance(Duration::from_secs(1));

        assert_eq!(clock.now(), Timestamp::MAX);
    }

    #[test]
    fn setting_replaces_the_instant() {
        let clock = FixedClock::default();
        assert_eq!(clock.now(), at(DEFAULT_NOW));

        clock.set(at("2030-05-05T05:05:05Z"));

        assert_eq!(clock.now(), at("2030-05-05T05:05:05Z"));
    }
}
