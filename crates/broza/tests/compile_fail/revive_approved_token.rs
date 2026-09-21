//! A token cannot be defaulted, duplicated or deserialised into existence.
//!
//! This file must NOT compile. Each line is one way of getting an `Approved`
//! without a decision behind it.

use broza::safety::guard::{Approved, Write};

fn assert_clone<T: Clone>() {}

fn default_one() {
    // There is no `Default`: a token always comes from a confirmed plan.
    let _token = Approved::<Write>::default();
}

fn duplicate_one() {
    // A token is spent once; it is deliberately not `Clone`.
    assert_clone::<Approved<Write>>();
}

fn deserialize_one() {
    // A token is not data: it cannot travel through JSON, a file or an IPC hop.
    let _token: Approved<Write> = serde_json::from_str("{}").unwrap_or_else(|e| panic!("{e}"));
}

fn main() {}
