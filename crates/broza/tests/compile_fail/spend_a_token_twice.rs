//! An approval is spent once.
//!
//! This file must NOT compile: `narrow_to_snapshot_delete` takes the token by
//! value, so the same approval cannot also be handed to a filesystem write.

use broza::safety::guard::{Approved, Write, narrow_to_snapshot_delete};

fn execute(_approved: &Approved<Write>) {}

fn spend_twice(approved: Approved<Write>) {
    let _snapshots = narrow_to_snapshot_delete(approved);
    execute(&approved);
}

fn main() {}
