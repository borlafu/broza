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
//! The modules below are write paths, and each one holds a token and re-checks
//! `(device, inode)` against it immediately before it writes:
//!
//! | Module | What it writes | Token |
//! |---|---|---|
//! | [`mover`] | the session directory, the manifest, one rename per item | [`Approved<Write>`](crate::safety::guard::Write) |
//! | [`restore`] | the manifest, one rename per entry, the emptied session | [`Approved<QuarantineWrite>`](crate::safety::guard::QuarantineWrite) for the sources and [`Approved<RestoreWrite>`](crate::safety::guard::RestoreWrite) for the destinations |
//! | [`expiry`] | `remove_tree` of a whole session | [`Approved<QuarantineWrite>`](crate::safety::guard::QuarantineWrite) |
//! | [`list`] | nothing | none |
//!
//! [`attempt`] holds the re-check for the move and [`guarded`] the one for
//! every write inside the store.
//!
//! # Two things can always disagree
//!
//! The manifest is a file and the items are files, and no filesystem updates
//! both at once. Every read therefore settles one against the other
//! ([`mod@reconcile`]), and nothing is ever deleted because the *manifest* says the
//! session is empty — only because the disk agrees.

pub mod attempt;
pub mod closing;
pub mod codes;
pub mod entries;
pub mod expiry;
#[cfg(test)]
mod expiry_tests;
#[cfg(test)]
mod fixtures;
pub mod guarded;
pub mod layout;
pub mod list;
pub mod lock;
pub mod manifest;
pub mod measure;
pub mod mover;
#[cfg(test)]
mod mover_tests;
pub mod putback;
pub mod reconcile;
pub mod report;
pub mod restore;
#[cfg(test)]
mod restore_tests;
pub mod selection;
pub mod store;
pub mod ttl;

pub use expiry::{all_sessions, expire, expired_sessions, purge};
pub use list::list_sessions;
pub use manifest::{MANIFEST_VERSION, Manifest};
pub use measure::measure_dir_bytes;
pub use mover::{MoveOutcome, MoveRequest, quarantine_items};
pub use reconcile::{Reconciled, reconcile, stored_items};
pub use report::Reported;
pub use restore::{restore_entries, restore_session, restore_wanted};
pub use selection::{Wanted, destinations, entry_destinations, group_by_session, session_destinations};
pub use store::{StoreContents, StoredSession};
