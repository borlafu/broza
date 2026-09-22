//! Cleanup: turning findings into a plan, and executing an approved one.
//!
//! The planner is pure: it never touches the filesystem. Execution is gated by the
//! `Approved<Write>` token produced by [`crate::safety::guard`], so nothing in this
//! module can write without passing the safety kernel first. [`executor`] hands
//! the reversible items to the quarantine mover and removes the irreversible
//! ones itself, re-checking each against the token immediately before.

pub mod executor;
#[cfg(all(test, feature = "test-support"))]
mod executor_tests;
pub mod planner;

pub use executor::{Executed, execute};
pub use planner::{PlanError, PlanOutcome, Selection, max_risk, plan_dry_run};
