//! Size and duration value types (`docs/cli-spec.md` §1.4).
//!
//! [`ByteSize`] accepts decimal (`KB`, `MB`, `GB`, `TB`) and binary (`KiB`, `MiB`,
//! `GiB`, `TiB`) units, case-insensitively and with an optional space. [`DurationSpec`]
//! accepts `<int><unit>` with `h`, `d`, `w`, `m` (30 days) and `y` (365 days).
//!
//! These are **configuration and command-line** value types, not part of the JSON
//! contract: they carry a unit suffix and serialise as strings. Payload structs in
//! [`crate::model`] always use plain `u64` byte counts, so [`ByteSize`] must never
//! appear in one.

pub mod bytes;
pub mod duration;

pub use bytes::ByteSize;
pub use duration::{DurationSpec, DurationUnit};
