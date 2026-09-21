//! The quarantine store: move, manifest, restore, expire, purge.

#[cfg(test)]
mod fixtures;
pub mod layout;
pub mod manifest;

pub use manifest::{MANIFEST_VERSION, Manifest};
