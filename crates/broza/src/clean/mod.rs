//! Cleanup: turning findings into a plan, and later executing an approved one.
//!
//! The planner is pure: it never touches the filesystem. Execution is gated by the
//! `Approved<Write>` token produced by [`crate::safety::guard`], so nothing in this
//! module can write without passing the safety kernel first.

pub mod planner;

pub use planner::{PlanError, Selection, max_risk, plan_dry_run};
