//! Adapters: the only place where Broza talks to macOS.
//!
//! This is the sole module allowed to spawn processes, call `libc`/`objc2`, or use
//! `unsafe` (`AGENTS.md` §4). Everything here implements a trait from
//! [`crate::ports`], so the rest of the core stays pure and testable.

pub(crate) mod io_error;
pub mod std_fs;

pub use std_fs::StdFileOps;
