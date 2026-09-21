//! Domain model. These serde types **are** the JSON contract (`docs/cli-spec.md` §4).
//!
//! Rules: sizes are `u64` bytes; timestamps are RFC 3339 UTC strings; enums are
//! `#[non_exhaustive]` and consumers ignore unknown fields.
//!
//! # Unknown enum values
//!
//! The specification requires *consumers* of the JSON to ignore unknown enum values.
//! Broza is the producer, so the model resolves this as follows:
//!
//! - Enums whose vocabulary already contains a catch-all value ([`VolumeRole`],
//!   [`FsKind`]) map any unknown string to their `Unknown` variant via
//!   `#[serde(other)]`; nothing is lost and nothing fails.
//! - The closed enums of the stable table ([`Risk`], [`Action`], [`ItemStatus`],
//!   [`ItemErrorCode`], [`SessionState`], [`Category`], [`ItemKind`],
//!   [`OperationKind`]) reject unknown values with a serde error. Inventing an
//!   `Unknown` variant for them would let Broza re-emit a value the contract does not
//!   define; a rejected document is loud, recoverable and never a panic.
//!
//! Unknown *fields* are always ignored, at every level.

pub mod category;
pub mod disk;
pub mod envelope;
pub mod finding;
pub mod finding_builder;
pub mod ids;
pub mod plan;
pub mod quarantine;
pub mod scan;
pub mod suggest;
pub mod units;

pub use category::{ALL_CATEGORIES, Category};
pub use disk::{Container, Disk, FsKind, Snapshot, Volume, VolumeRole};
pub use envelope::{Envelope, ErrorEntry, Host, Warning};
pub use finding::{Action, Finding, FindingPath, Instructions, Risk};
pub use finding_builder::FindingBuilder;
pub use ids::{FindingId, SessionId, VolumeId};
pub use plan::{CleanItem, CleanPlan, ExpiredSession, ItemErrorCode, ItemStatus};
pub use quarantine::{
    EntryStatus, OperationKind, QuarantineEntry, QuarantineList, QuarantineSession, ReclaimReport,
    ReclaimSession, RestoreReport, RestoreSession, SessionState,
};
pub use scan::{ItemKind, LargestItem, ScanReport};
pub use suggest::{RiskTotals, SuggestReport};
pub use units::{ByteSize, DurationSpec, DurationUnit};
