//! Domain model. These serde types **are** the JSON contract (`docs/cli-spec.md` §4).
//!
//! Rules: sizes are `u64` bytes; timestamps are RFC 3339 UTC strings; enums are
//! `#[non_exhaustive]` and consumers ignore unknown fields.

pub mod envelope;

pub use envelope::{Envelope, ErrorEntry, Host, Warning};
