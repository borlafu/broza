//! An `Approved` token must be impossible to build outside `safety::guard`.
//!
//! This file must NOT compile. It is the executable form of the invariant in
//! `AGENTS.md` §2: "every mutating function requires an `Approved<_>` token that
//! only `crates/broza/src/safety/guard.rs` can construct".

#![allow(unreachable_code)]

use broza::safety::guard::{Approved, PendingApproval, Write};

fn build_a_token() {
    // Every field of `Approved` is private, including the seal.
    let _token: Approved<Write> = Approved { payload: todo!(), seal: todo!(), kind: todo!() };
}

fn skip_the_prompt() {
    // The pending approval is built by the guard, never by a caller, so nobody
    // can invent one and hand it a "yes".
    let _pending =
        PendingApproval { payload: todo!(), mode: todo!(), request: todo!(), seal: todo!() };
}

fn main() {}
