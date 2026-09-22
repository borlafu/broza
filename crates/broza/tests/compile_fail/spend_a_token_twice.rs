//! An approval is spent once.
//!
//! This file must NOT compile: `Approved::into_plan` takes the token by value,
//! so the same approval cannot also be handed to a filesystem write afterwards.

use broza::safety::guard::{Approved, Write};

fn execute(_approved: &Approved<Write>) {}

fn spend_twice(approved: Approved<Write>) {
    let _plan = approved.into_plan();
    execute(&approved);
}

fn main() {}
