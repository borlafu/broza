//! Retention arithmetic: when a session is past its time to live.
//!
//! The manifest stores `expires_at`, but the answer is always recomputed from
//! `created_at` plus the configured `quarantine-ttl`. A user who shortens the TTL
//! expects the change to apply to what is already in the store; a stored
//! `expires_at` frozen at write time would ignore them.

use std::time::Duration;

use jiff::{SignedDuration, Timestamp};

/// When a session created at `created_at` becomes eligible for expiry.
///
/// Saturates at [`Timestamp::MAX`]: an absurd TTL postpones expiry forever
/// instead of wrapping into the past.
pub fn expires_at(created_at: Timestamp, ttl: Duration) -> Timestamp {
    let step = SignedDuration::try_from(ttl).unwrap_or(SignedDuration::MAX);
    created_at.checked_add(step).unwrap_or(Timestamp::MAX)
}

/// `true` when the retention period of a session created at `created_at` is over.
pub fn is_past_ttl(created_at: Timestamp, ttl: Duration, now: Timestamp) -> bool {
    expires_at(created_at, ttl) < now
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use jiff::Timestamp;

    use super::{expires_at, is_past_ttl};
    use crate::quarantine::fixtures::at;

    /// Thirty days, the default `quarantine-ttl`.
    const THIRTY_DAYS: Duration = Duration::from_secs(30 * 24 * 60 * 60);

    #[test]
    fn a_session_expires_one_ttl_after_it_was_created() {
        let created = at("2026-09-21T10:36:08Z");

        assert_eq!(expires_at(created, THIRTY_DAYS), at("2026-10-21T10:36:08Z"));
    }

    #[test]
    fn a_session_is_past_its_ttl_only_after_the_instant_itself() {
        let created = at("2026-09-21T10:36:08Z");

        assert!(!is_past_ttl(created, THIRTY_DAYS, at("2026-10-21T10:36:08Z")), "not a moment sooner");
        assert!(is_past_ttl(created, THIRTY_DAYS, at("2026-10-21T10:36:09Z")));
    }

    #[test]
    fn an_absurd_retention_period_postpones_expiry_instead_of_wrapping() {
        let created = at("2026-09-21T10:36:08Z");

        assert_eq!(expires_at(created, Duration::MAX), Timestamp::MAX);
        assert!(!is_past_ttl(created, Duration::MAX, Timestamp::MAX));
    }
}
