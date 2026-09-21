//! Time source.

use jiff::Timestamp;

/// Provides the current time. Injected so tests can freeze it.
pub trait Clock: Send + Sync {
    /// Current instant.
    fn now(&self) -> Timestamp;
}
