//! Broza core engine.
//!
//! Safe, explainable disk analysis and cleanup for macOS. This crate has no
//! terminal, argument-parsing, or environment access: the CLI (and later the
//! GUI) wire concrete adapters into it and render the returned data.
//!
//! Safety invariants are documented in `AGENTS.md` §2 and `docs/prd.md` §7.3.

pub mod config;
pub mod error;
pub mod model;
pub mod ports;
pub mod safety;
pub mod scan;
pub mod units;

pub use error::BrozaError;
pub use safety::exit_code::ExitCode;

/// Version of the JSON contract produced by this crate (see `docs/cli-spec.md` §4).
pub const SCHEMA_VERSION: &str = "1.1";

/// Crate version, as published.
pub const BROZA_VERSION: &str = env!("CARGO_PKG_VERSION");
