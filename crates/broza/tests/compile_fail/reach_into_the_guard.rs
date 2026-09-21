//! The internals of the guard are not a public API.
//!
//! This file must NOT compile. Each line is a way of stepping around the one
//! entry point (`approve` → `confirm`) that produces a token.

use broza::safety::guard::{ApprovedPlan, PendingApproval};

fn seal_without_a_prompt(pending: PendingApproval) {
    // `seal` is private: an answer only ever comes from the prompt `confirm` ran.
    let _token = pending.seal(broza::ports::Answer::Yes);
}

fn build_a_payload() {
    // `ApprovedPlan::new` is `pub(super)`: only the guard pairs a plan with evidence.
    let _payload = ApprovedPlan::new(todo!(), Vec::new());
}

fn issue_a_token() {
    // The token factory is not even nameable from outside.
    let _token = broza::safety::guard::token::issue::<broza::safety::guard::Write>(todo!());
}

fn main() {}
