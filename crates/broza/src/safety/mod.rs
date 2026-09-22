//! Safety kernel: the only place in Broza that authorises a write.
//!
//! See `docs/adr/0003-approved-token-safety-kernel.md` and `docs/cli-spec.md` §3.4.
//!
//! # Invariants enforced here
//!
//! 1. **Dry run by default.** Without `--apply`, [`guard::approve`] can only ever
//!    return [`Verdict::DryRun`] or [`Verdict::Nothing`]; no token exists, so
//!    nothing can be written.
//! 2. **Quarantine by default.** `--purge` requires the literal word `PURGE`
//!    ([`policy::PURGE_LITERAL`]) typed through
//!    [`Prompter::confirm_literal`](crate::ports::Prompter::confirm_literal), and
//!    ignores `--yes`; combining them is refused.
//! 3. **Protected volumes are read-only.** [`roles::allows_action`] refuses
//!    `system`, `preboot`, `recovery` and `vm`, and any role it does not know.
//!    No flag changes this and none will be added.
//! 4. **Explicit confirmation.** [`policy::confirmation_policy`] decides the mode;
//!    "no" is exit `6` and "no TTY" is exit `7`.
//! 5. **`inform_only` is never cleaned** (exit `2`), whether it reached the plan
//!    as an item or only as a selected finding.
//! 6. **Paths are checked, not trusted**: absolute, free of `.` and `..`, every
//!    component `lstat`-ed and free of symlinks, resolved to a volume through the
//!    firmlink-aware [`crate::scan::MountTable`], inside an allowed root, not
//!    excluded, within `--max-size`, and carrying exactly the action the finding
//!    plus `--purge` imply.
//!
//! # What the kernel does *not* guarantee
//!
//! Directory sizes are the scanner's aggregate, not something the kernel
//! measured: `lstat` on a directory reports the directory entry, and walking
//! every subtree again would double the cost of a run.
//! [`ApprovedItem::size_verified`](guard::ApprovedItem::size_verified) marks
//! which figure is which, and the executor re-measures a directory immediately
//! before removing it (`docs/cli-spec.md` §3.4, check 6).
//!
//! The checks describe the filesystem **at the moment they ran**. A path can be
//! replaced between the check and the write, and no user-space program can
//! prevent that. The kernel therefore hands the executor the `(device, inode)` it
//! saw ([`guard::ApprovedItem`]), and every mutating function must `lstat` again
//! and skip the item when the pair no longer matches. Approval is evidence, not a
//! promise about the future.
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
//! the complete list of write paths in the code base. The second audit surface is
//! [`crate::ports::FileOps`]: its mutating methods may only be called from
//! `clean::executor` and `quarantine::*`, and each of those call sites must hold a
//! token. `trybuild` cases in `crates/broza/tests/compile_fail/` prove the token
//! cannot be forged, cloned, defaulted or deserialised.
//!
//! [`Verdict::DryRun`]: guard::Verdict::DryRun
//! [`Verdict::Nothing`]: guard::Verdict::Nothing

pub mod exclusions;
pub mod exit_code;
pub mod firmlink;
pub mod guard;
pub mod path;
pub mod policy;
pub mod rejection;
pub mod roles;
pub mod roots;

pub use exclusions::Exclusions;
pub use exit_code::ExitCode;
pub use guard::{
    Approved, ApprovedItem, ApprovedPlan, PendingApproval, QuarantineWrite, RestoreRequest, RestoreWrite,
    SnapshotDelete, Verdict, Write, WriteKind, WriteRequest, approve, approve_quarantine_write,
    approve_restore_targets,
};
pub use path::{CanonicalPath, canonicalize_no_follow};
pub use policy::{ConfirmationMode, PURGE_LITERAL, PolicyInput, RejectReason, confirmation_policy};
pub use rejection::{GuardRejection, PolicyError, UnreadableCause};
pub use roles::{allows_action, is_protected};
pub use roots::{AllowedRoots, RootContext, is_under_allowed_root};
