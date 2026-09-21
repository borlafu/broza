//! Real [`Clock`](crate::ports::Clock).

use jiff::Timestamp;

use crate::ports::Clock;

/// The wall clock of the machine.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::SystemClock;
    use crate::ports::Clock;

    /// Any moment before which the machine's clock is obviously wrong.
    const SANITY_FLOOR: &str = "2020-01-01T00:00:00Z";

    #[test]
    fn the_system_clock_reports_a_plausible_instant() {
        let floor = SANITY_FLOOR.parse::<Timestamp>().unwrap_or_else(|e| panic!("{e}"));

        assert!(SystemClock.now() > floor);
    }

    #[test]
    fn the_system_clock_never_goes_backwards() {
        let first = SystemClock.now();
        let second = SystemClock.now();

        assert!(second >= first);
    }
}
