//! An `Approved` token must be impossible to build outside `safety::guard`.
//!
//! This file must NOT compile. It is the executable form of the invariant in
//! `AGENTS.md` §2: "every mutating function requires an `Approved<_>` token that
//! only `crates/broza/src/safety/guard.rs` can construct".

#![allow(unreachable_code)]

use broza::safety::guard::{Approved, Write};

fn main() {
    let _token: Approved<Write> = Approved { payload: todo!(), seal: todo!(), kind: todo!() };
}
