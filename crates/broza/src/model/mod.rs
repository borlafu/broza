//! Domain model. These serde types **are** the JSON contract (`docs/cli-spec.md` §4).
//!
//! Rules: sizes are `u64` bytes; timestamps are RFC 3339 UTC strings; enums are
//! `#[non_exhaustive]` and consumers ignore unknown fields. Value types that carry a
//! unit (`50MB`, `30d`) live in [`crate::units`] and never appear in a payload.
//!
//! # Unknown enum values
//!
//! The specification requires consumers to ignore unknown enum values. Broza is both
//! a producer and — when it reads back a quarantine manifest possibly written by a
//! newer version — a consumer, so the model splits its enums in three:
//!
//! - **Open enums**: [`FsKind`], [`ItemStatus`], [`ItemErrorCode`] and
//!   [`SessionState`] carry `Unknown(String)`. An unrecognised token survives a
//!   read-modify-write cycle verbatim, so an older Broza never corrupts a manifest
//!   written by a newer one. Use `is_known()` before acting on such a value.
//! - **Collapsing enum**: [`VolumeRole`] maps anything unrecognised to
//!   [`VolumeRole::Unknown`]. The role gates write protection (`AGENTS.md` §2.3), so
//!   an unreadable role must behave like the most restrictive one, not round-trip.
//! - **Closed enums**: [`Risk`], [`Action`], [`Category`], [`ItemKind`] and
//!   [`OperationKind`] only ever come from Broza itself and reject unknown tokens
//!   with a serde error — loud, recoverable and never a panic.
//!
//! Unknown *fields* are always ignored, at every level.
//!
//! # Validated types
//!
//! [`Finding`] and [`CleanPlan`] have private fields. Both are built through a
//! builder or parsed through a private mirror struct, and both paths end in
//! `validate()`, so an invariant of the specification cannot be broken by
//! constructing, parsing or mutating one.

pub mod category;
pub mod disk;
pub mod envelope;
pub mod finding;
pub mod finding_builder;
pub mod ids;
mod open_enum;
pub mod plan;
pub mod plan_item;
pub mod quarantine;
pub mod scan;
pub mod status;
pub mod suggest;
pub(crate) mod timestamp;

pub use category::Category;
pub use disk::{Container, Disk, FsKind, Snapshot, TIME_MACHINE_PREFIX, Volume, VolumeRole};
pub use envelope::{Diagnostic, Envelope, ErrorEntry, Host, Warning};
pub use finding::{Action, Finding, FindingPath, Instructions, Risk};
pub use finding_builder::FindingBuilder;
pub use ids::{EntryId, FindingId, SessionId, VolumeId};
pub use plan::{CleanPlan, CleanPlanRepr};
pub use plan_item::{CleanItem, ExpiredSession, SnapshotRef};
pub use quarantine::{
    EntryStatus, OperationKind, QuarantineEntry, QuarantineList, QuarantineSession, ReclaimReport,
    ReclaimSession, RestoreReport, RestoreSession, SessionState,
};
pub use scan::{ItemKind, LargestItem, ScanReport};
pub use status::{ItemErrorCode, ItemStatus};
pub use suggest::{RiskTotals, SuggestReport};
