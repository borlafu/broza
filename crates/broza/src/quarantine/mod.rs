//! The quarantine store: move, manifest, restore, expire, purge.
//!
//! `clean --apply` never deletes: it *moves* items here (`AGENTS.md` §2.2), and
//! space is reclaimed later by `broza quarantine expire` or `purge`. The layout
//! is the one ADR 0004 fixes:
//!
//! ```text
//! ~/.local/share/broza/quarantine/
//!   cln_20260921103608_a1b2/
//!     manifest.json           { "manifest_version": 1, "session": { ... } }
//!     items/0001/<basename>
//! ```
//!
//! # Audit surface
//!
//! Together with `clean::executor` and `scan::cache::store`, the modules below
//! are the only callers of the four mutating methods of
//! [`FileOps`](crate::ports::FileOps) — `rename`, `remove_tree`,
//! `create_dir_all` and `write_atomic`:
//!
//! | Module | What it writes | Token |
//! |---|---|---|
//! | [`mover`] | the session directory, the manifest, one `rename` per item | [`Approved<Write>`](crate::safety::guard::Write) |
//! | [`restore`] | the manifest, one `rename` per entry, the emptied session | [`Approved<QuarantineWrite>`](crate::safety::guard::QuarantineWrite) |
//! | [`expiry`] | `remove_tree` of a whole session | [`Approved<QuarantineWrite>`](crate::safety::guard::QuarantineWrite) |
//! | [`list`] | nothing | none |
//!
//! Every one of them re-`lstat`s the path and compares `(device, inode)` with
//! the pair the guard recorded before it writes; see
//! [`attempt`] for the move and [`guarded`] for the rest.

pub mod attempt;
pub mod codes;
pub mod entries;
pub mod expiry;
#[cfg(test)]
mod fixtures;
pub mod guarded;
pub mod layout;
pub mod list;
pub mod manifest;
pub mod measure;
pub mod mover;
pub mod putback;
pub mod report;
pub mod restore;
pub mod store;
pub mod ttl;

pub use expiry::{all_sessions, expire, expired_sessions, purge};
pub use list::list_sessions;
pub use manifest::{MANIFEST_VERSION, Manifest};
pub use measure::measure_dir_bytes;
pub use mover::{MoveOutcome, MoveRequest, quarantine_items};
pub use report::Reported;
pub use restore::{restore_entries, restore_session};
pub use store::StoredSession;
