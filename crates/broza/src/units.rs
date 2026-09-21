//! Size and duration value types (`docs/cli-spec.md` §1.4).
//!
//! [`ByteSize`] accepts decimal (`KB`, `MB`, `GB`, `TB`) and binary (`KiB`, `MiB`,
//! `GiB`, `TiB`) units, case-insensitively and with an optional space. [`DurationSpec`]
//! accepts `<int><unit>` with `h`, `d`, `w`, `m` (30 days) and `y` (365 days).
//!
//! Both are pure value types: the JSON contract always carries integer bytes, so these
//! types only appear in configuration and on the command line.

pub mod bytes;
pub mod duration;

pub use bytes::ByteSize;
pub use duration::{DurationSpec, DurationUnit};
