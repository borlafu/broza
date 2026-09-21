//! `clean` payload: the plan and its items (`docs/cli-spec.md` §4.4).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::BrozaError;
use crate::model::finding::Action;
use crate::model::ids::{FindingId, SessionId};

/// Payload of `broza clean`, in dry-run and in `--apply` mode alike.
///
/// The byte counters are normative (`docs/cli-spec.md` §4.4): `quarantined_bytes` are
/// still on disk until expiry or purge and are never added to `reclaimed_bytes`
/// (`AGENTS.md` §2.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CleanPlan {
    /// `true` when nothing was written to disk.
    pub dry_run: bool,
    /// Identifier of the cleanup session.
    pub session_id: SessionId,
    /// Sum of `size_bytes` of every item in the plan, whatever its outcome.
    pub planned_bytes: u64,
    /// Bytes moved into quarantine in this run. Pending, not yet reclaimed.
    pub quarantined_bytes: u64,
    /// Bytes actually freed in this run: purges, `tmutil` deletions and expired sessions.
    pub reclaimed_bytes: u64,
    /// Directory holding the quarantined items. Absent in a dry run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine_path: Option<PathBuf>,
    /// Sessions expired during the pre-execution step; their bytes count as reclaimed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expired_sessions: Vec<ExpiredSession>,
    /// The items of the plan.
    #[serde(default)]
    pub items: Vec<CleanItem>,
}

impl CleanPlan {
    /// Check the invariants of `docs/cli-spec.md` §4.4.
    ///
    /// In a dry run nothing is quarantined or reclaimed, every item is `planned` and no
    /// quarantine directory exists. In every mode an item carries an `error` only when
    /// it was skipped or failed.
    ///
    /// The `planned_bytes` total is checked separately by
    /// [`CleanPlan::planned_bytes_match_items`]: only a plan holding all of its items
    /// can satisfy it.
    pub fn validate(&self) -> Result<(), BrozaError> {
        let inconsistent =
            |reason: &str| Err(BrozaError::Other(format!("clean plan `{}`: {reason}", self.session_id)));
        if self.items.iter().any(|item| item.error.is_some() && !item.status.is_unsuccessful()) {
            return inconsistent("only skipped or failed items carry an `error`");
        }
        if !self.dry_run {
            return Ok(());
        }
        if self.quarantined_bytes != 0 || self.reclaimed_bytes != 0 || self.quarantine_path.is_some() {
            return inconsistent("a dry run neither quarantines nor reclaims bytes");
        }
        if self.items.iter().any(|item| item.status != ItemStatus::Planned) {
            return inconsistent("every item of a dry run is `planned`");
        }
        Ok(())
    }

    /// `true` when `planned_bytes` is the sum of the sizes of `items`.
    ///
    /// Always true for a plan the planner produced; false for an abridged document such
    /// as the example in the specification.
    pub fn planned_bytes_match_items(&self) -> bool {
        let planned = self.items.iter().try_fold(0_u64, |sum, item| sum.checked_add(item.size_bytes));
        planned == Some(self.planned_bytes)
    }
}

/// A quarantine session expired before the plan ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ExpiredSession {
    /// Identifier of the expired session.
    pub id: SessionId,
    /// Bytes freed by deleting it.
    pub freed_bytes: u64,
}

/// One item of a [`CleanPlan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CleanItem {
    /// Absolute path of the item.
    pub path: PathBuf,
    /// Finding the item came from.
    pub finding_id: FindingId,
    /// Size of the item in bytes.
    pub size_bytes: u64,
    /// Outcome of the item.
    pub status: ItemStatus,
    /// Action that was planned or executed.
    pub action: Action,
    /// Why the item was skipped or failed. Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ItemErrorCode>,
}

/// Outcome of a clean, restore or quarantine item (`docs/cli-spec.md` §4.1, `status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ItemStatus {
    /// In the plan, not executed (every item of a dry run).
    Planned,
    /// Moved into the quarantine store.
    Quarantined,
    /// Deleted irreversibly.
    Purged,
    /// Moved back to its original path.
    Restored,
    /// Not attempted; see `error`.
    Skipped,
    /// Attempted and failed; see `error`.
    Failed,
}

impl ItemStatus {
    /// `true` for the statuses that carry an [`ItemErrorCode`]: `skipped` and `failed`.
    pub const fn is_unsuccessful(self) -> bool {
        matches!(self, Self::Skipped | Self::Failed)
    }
}

/// Item-level error code (`docs/cli-spec.md` §4.1, `error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ItemErrorCode {
    /// The item is on another volume than its quarantine directory.
    CrossVolume,
    /// Broza is not allowed to read or move the item.
    PermissionDenied,
    /// Something already exists at the destination.
    Collision,
    /// The item disappeared between planning and execution.
    NotFound,
    /// The item lives on a volume Broza must not write to (`AGENTS.md` §2.3).
    ProtectedVolume,
    /// Another Broza process holds the session.
    SessionBusy,
    /// Any other I/O failure.
    IoError,
}

#[cfg(test)]
mod tests {
    use super::{CleanItem, CleanPlan, ItemErrorCode, ItemStatus};
    use crate::model::finding::Action;
    use crate::model::ids::SessionId;

    fn session() -> SessionId {
        "cln_20260921103608_a1b2".parse().unwrap_or_else(|e| panic!("{e}"))
    }

    fn item(size_bytes: u64, status: ItemStatus) -> CleanItem {
        CleanItem {
            path: "/Users/x/Library/Caches/example".into(),
            finding_id: "user-cache.logs".parse().unwrap_or_else(|e| panic!("{e}")),
            size_bytes,
            status,
            action: Action::Quarantine,
            error: None,
        }
    }

    fn dry_run_plan(items: Vec<CleanItem>) -> CleanPlan {
        CleanPlan {
            dry_run: true,
            session_id: session(),
            planned_bytes: items.iter().map(|i| i.size_bytes).sum(),
            quarantined_bytes: 0,
            reclaimed_bytes: 0,
            quarantine_path: None,
            expired_sessions: Vec::new(),
            items,
        }
    }

    #[test]
    fn a_dry_run_plan_is_valid_when_nothing_moved() {
        let plan = dry_run_plan(vec![item(10, ItemStatus::Planned), item(32, ItemStatus::Planned)]);
        assert_eq!(plan.planned_bytes, 42);
        assert!(plan.validate().is_ok());
    }

    #[test]
    fn a_dry_run_that_reports_movement_is_rejected() {
        let base = dry_run_plan(vec![item(10, ItemStatus::Planned)]);

        let quarantined = CleanPlan { quarantined_bytes: 10, ..base.clone() };
        assert!(quarantined.validate().is_err());

        let reclaimed = CleanPlan { reclaimed_bytes: 10, ..base.clone() };
        assert!(reclaimed.validate().is_err());

        let with_path = CleanPlan { quarantine_path: Some("/tmp/q".into()), ..base.clone() };
        assert!(with_path.validate().is_err());

        let executed = CleanPlan { items: vec![item(10, ItemStatus::Quarantined)], ..base };
        assert!(executed.validate().is_err());
    }

    #[test]
    fn planned_bytes_are_checked_against_the_items_separately() {
        let plan = dry_run_plan(vec![item(10, ItemStatus::Planned), item(32, ItemStatus::Planned)]);
        assert!(plan.planned_bytes_match_items());
        let abridged = CleanPlan { planned_bytes: 99, ..plan };
        assert!(!abridged.planned_bytes_match_items());
        assert!(abridged.validate().is_ok(), "an abridged total is not a structural error");
    }

    #[test]
    fn only_skipped_or_failed_items_may_carry_an_error() {
        for status in [ItemStatus::Skipped, ItemStatus::Failed] {
            assert!(status.is_unsuccessful(), "{status:?}");
            let failed = CleanItem { error: Some(ItemErrorCode::CrossVolume), ..item(10, status) };
            let plan = CleanPlan { dry_run: false, ..dry_run_plan(vec![failed]) };
            assert!(plan.validate().is_ok(), "{status:?}");
        }
        for status in [ItemStatus::Planned, ItemStatus::Quarantined, ItemStatus::Purged, ItemStatus::Restored]
        {
            assert!(!status.is_unsuccessful(), "{status:?}");
            let bogus = CleanItem { error: Some(ItemErrorCode::Collision), ..item(10, status) };
            let plan = CleanPlan { dry_run: false, ..dry_run_plan(vec![bogus]) };
            assert!(plan.validate().is_err(), "{status:?}");
        }
    }

    #[test]
    fn an_applied_plan_may_quarantine_and_reclaim() {
        let plan = CleanPlan {
            dry_run: false,
            quarantined_bytes: 10,
            reclaimed_bytes: 5,
            quarantine_path: Some("/Users/x/.local/share/broza/quarantine".into()),
            ..dry_run_plan(vec![item(10, ItemStatus::Quarantined)])
        };
        assert!(plan.validate().is_ok());
    }

    #[test]
    fn statuses_and_error_codes_use_the_stable_identifiers() {
        let statuses = [
            (ItemStatus::Planned, "\"planned\""),
            (ItemStatus::Quarantined, "\"quarantined\""),
            (ItemStatus::Purged, "\"purged\""),
            (ItemStatus::Restored, "\"restored\""),
            (ItemStatus::Skipped, "\"skipped\""),
            (ItemStatus::Failed, "\"failed\""),
        ];
        for (status, expected) in statuses {
            assert_eq!(serde_json::to_string(&status).unwrap_or_else(|e| panic!("{e}")), expected);
        }
        let codes = [
            (ItemErrorCode::CrossVolume, "\"cross_volume\""),
            (ItemErrorCode::PermissionDenied, "\"permission_denied\""),
            (ItemErrorCode::Collision, "\"collision\""),
            (ItemErrorCode::NotFound, "\"not_found\""),
            (ItemErrorCode::ProtectedVolume, "\"protected_volume\""),
            (ItemErrorCode::SessionBusy, "\"session_busy\""),
            (ItemErrorCode::IoError, "\"io_error\""),
        ];
        for (code, expected) in codes {
            assert_eq!(serde_json::to_string(&code).unwrap_or_else(|e| panic!("{e}")), expected);
        }
        assert!(serde_json::from_str::<ItemErrorCode>("\"meteorite\"").is_err());
    }

    #[test]
    fn an_item_without_an_error_omits_the_field() {
        let json = serde_json::to_value(item(10, ItemStatus::Planned)).unwrap_or_else(|e| panic!("{e}"));
        assert!(json.get("error").is_none());
    }
}
