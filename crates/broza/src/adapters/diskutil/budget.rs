//! One time budget shared by every command of a single enumeration.
//!
//! A per-command timeout bounds each `diskutil` call, but an enumeration issues
//! one call per disk and per partition, so a machine where every call is merely
//! slow could still keep the caller waiting for minutes. The budget bounds the
//! whole operation: each command gets whatever is left, and once nothing is
//! left the enumeration fails instead of starting another command.

use std::time::{Duration, Instant};

use crate::BrozaError;

/// Deadline shared by every command of one enumeration.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Budget {
    /// When the budget is spent.
    at: Instant,
    /// The budget itself, for the error message.
    total: Duration,
}

impl Budget {
    /// Start a budget of `total`, now.
    ///
    /// A duration so large that adding it overflows the clock is clamped to
    /// "now", which simply means the first command gets no time — never a
    /// panic.
    pub(crate) fn start(total: Duration) -> Self {
        let now = Instant::now();
        Self { at: now.checked_add(total).unwrap_or(now), total }
    }

    /// The timeout for the next command: `limit`, or less when time is short.
    pub(crate) fn next_timeout(&self, limit: Duration) -> Result<Duration, BrozaError> {
        let remaining = self.at.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(BrozaError::Other(format!(
                "enumerating the disks took longer than {} s and was given up",
                self.total.as_secs()
            )));
        }
        Ok(remaining.min(limit))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::Budget;
    use crate::BrozaError;

    #[test]
    fn a_fresh_budget_gives_a_command_its_full_limit() {
        let budget = Budget::start(Duration::from_secs(60));

        let timeout = budget.next_timeout(Duration::from_secs(20)).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(timeout, Duration::from_secs(20));
    }

    #[test]
    fn a_budget_smaller_than_the_limit_shortens_the_command() {
        let budget = Budget::start(Duration::from_millis(50));

        let timeout = budget.next_timeout(Duration::from_secs(20)).unwrap_or_else(|e| panic!("{e}"));

        assert!(timeout <= Duration::from_millis(50), "{timeout:?}");
        assert!(!timeout.is_zero());
    }

    #[test]
    fn a_spent_budget_refuses_to_start_another_command() {
        let budget = Budget::start(Duration::ZERO);

        let err = budget.next_timeout(Duration::from_secs(20)).err();

        let Some(BrozaError::Other(message)) = err else { panic!("expected BrozaError::Other") };
        assert!(message.contains("longer than 0 s"), "{message}");
    }

    #[test]
    fn an_absurd_budget_does_not_overflow_the_clock() {
        let budget = Budget::start(Duration::MAX);

        assert!(budget.next_timeout(Duration::from_secs(1)).is_err(), "clamped to now, not panicking");
    }
}
