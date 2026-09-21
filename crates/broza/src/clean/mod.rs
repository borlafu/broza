//! Cleanup: turning findings into a plan, and later executing an approved one.
//!
//! The planner is pure: it never touches the filesystem. Execution is gated by the
//! `Approved<Write>` token produced by [`crate::safety::guard`], so nothing in this
//! module can write without passing the safety kernel first.

// TODO(M3): the executor must re-measure every directory item immediately before
// removing it and abandon the item when `--max-size` would be exceeded. The guard
// only verifies file sizes: `lstat` on a directory reports the directory entry,
// not the subtree, and walking trees inside the safety kernel would double the
// cost of every run. `ApprovedItem::size_verified()` says which figure was
// measured (`docs/cli-spec.md` §3.4, check 6).

pub mod planner;

pub use planner::{PlanError, PlanOutcome, Selection, max_risk, plan_dry_run};
