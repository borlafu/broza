//! Safety kernel: the only place in Broza that authorises a write.
//!
//! See `docs/adr/0003-approved-token-safety-kernel.md` and `docs/cli-spec.md` §3.4.
//!
//! # Invariants enforced here
//!
//! 1. **Dry run by default.** Without `--apply`, [`guard::approve`] can only ever
//!    return [`Verdict::DryRun`]; no token exists, so nothing can be written.
//! 2. **Quarantine by default.** `--purge` requires the literal word `PURGE`
//!    ([`policy::PURGE_LITERAL`]) and ignores `--yes`; combining them is refused.
//! 3. **Protected volumes are read-only.** [`roles::allows_action`] refuses
//!    `system`, `preboot`, `recovery` and `vm`, and any role it does not know.
//!    No flag changes this and none will be added.
//! 4. **Explicit confirmation.** [`policy::confirmation_policy`] decides the mode;
//!    "no" is exit `6` and "no TTY" is exit `7`.
//! 5. **`inform_only` rejects the whole plan** (exit `2`), so `cloud-synced` is
//!    never removed.
//! 6. **Paths are proved before they are used**: absolute, lexically normalised,
//!    every component `lstat`-ed and free of symlinks, resolved to a volume through
//!    the firmlink-aware [`crate::scan::MountTable`], inside an allowed root, not
//!    excluded, and within `--max-size`.
//!
//! # Audit rule
//!
//! [`Approved<K>`](guard::Approved) carries a private seal that only
//! `safety::guard` can name, so no other module — and no other crate — can build
//! one. Every mutating function takes `&Approved<_>`, which makes
//!
//! ```text
//! grep -rn "Approved<" crates/
//! ```
//!
//! the complete list of write paths in the code base. A `trybuild` compile-fail
//! test (`crates/broza/tests/compile_fail/`) proves the token cannot be forged.
//!
//! [`Verdict::DryRun`]: guard::Verdict::DryRun

pub mod exclusions;
pub mod exit_code;
pub mod guard;
pub mod path;
pub mod policy;
pub mod rejection;
pub mod roles;
#[cfg(test)]
pub(crate) mod test_fs;

pub use exclusions::Exclusions;
pub use exit_code::ExitCode;
pub use guard::{
    Approved, PendingApproval, QuarantineWrite, SnapshotDelete, Verdict, Write, WriteKind, WriteRequest,
    approve, approve_quarantine_write, narrow_to_snapshot_delete,
};
pub use path::{AllowedRoots, RootContext, canonicalize_no_follow, is_under_allowed_root};
pub use policy::{ConfirmationMode, PURGE_LITERAL, PolicyInput, RejectReason, confirmation_policy};
pub use rejection::{GuardRejection, PolicyError};
pub use roles::{allows_action, is_protected};
