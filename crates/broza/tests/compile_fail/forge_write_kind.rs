//! A foreign crate must not be able to add a new kind of write.
//!
//! This file must NOT compile: `WriteKind` is sealed, so no type outside
//! `safety::guard` can claim to authorise a write.

use broza::safety::guard::WriteKind;

struct Forged;

impl WriteKind for Forged {
    type Payload = ();
    const DESCRIPTION: &'static str = "forged";
}

fn main() {}
