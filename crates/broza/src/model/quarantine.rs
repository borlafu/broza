//! Quarantine store payloads (`docs/cli-spec.md` §4.5 and §4.6).

use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::model::ids::SessionId;
use crate::model::plan::{ItemErrorCode, ItemStatus};

/// Status of a quarantine entry.
///
/// The specification has a single stable `status` enum for clean, restore and
/// quarantine items, so this is [`ItemStatus`] under the name used in §4.6.
pub type EntryStatus = ItemStatus;

/// Payload of `broza quarantine list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct QuarantineList {
    /// Root of the quarantine store.
    pub quarantine_path: PathBuf,
    /// Bytes held by every session.
    pub total_bytes: u64,
    /// Bytes held by sessions whose `expires_at` has passed.
    pub expired_bytes: u64,
    /// Sessions in the store, newest first.
    #[serde(default)]
    pub sessions: Vec<QuarantineSession>,
}

/// One cleanup session in the quarantine store; also the shape of its `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct QuarantineSession {
    /// Identifier of the session.
    pub id: SessionId,
    /// When the session was created.
    pub created_at: Timestamp,
    /// When the session becomes eligible for expiry.
    pub expires_at: Timestamp,
    /// Bytes held by the session.
    pub total_bytes: u64,
    /// Number of entries in the session.
    pub item_count: u64,
    /// Lifecycle state of the session.
    pub state: SessionState,
    /// Entries of the session. Listed in the manifest, omitted from `quarantine list`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<QuarantineEntry>,
}

/// Lifecycle state of a [`QuarantineSession`] (`docs/cli-spec.md` §4.1, `state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionState {
    /// Items are still being moved into the session.
    InProgress,
    /// Every item was moved successfully.
    Complete,
    /// Items are being moved back to their original paths.
    Restoring,
    /// Derived at read time from `expires_at < now`. Never written to the manifest.
    Expired,
}

/// One item inside a quarantine session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct QuarantineEntry {
    /// Identifier of the entry: `<session id>/<seq>`.
    pub id: String,
    /// Where the item came from.
    pub original_path: PathBuf,
    /// Where the item is stored inside the session directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored_path: Option<PathBuf>,
    /// Where the item was put back. Present after a restore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored_to: Option<PathBuf>,
    /// Size of the item in bytes.
    pub size_bytes: u64,
    /// Outcome of the entry.
    pub status: EntryStatus,
    /// Why the entry was skipped or failed. Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ItemErrorCode>,
}

/// Which operation produced a report (`docs/cli-spec.md` §4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OperationKind {
    /// `broza restore`.
    Restore,
    /// `broza quarantine expire`.
    Expire,
    /// `broza quarantine purge`.
    Purge,
}

/// Payload of `broza restore`, including `restore --list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RestoreReport {
    /// Always [`OperationKind::Restore`].
    pub operation: OperationKind,
    /// Bytes moved back to their original paths. `0` for `restore --list`.
    pub restored_bytes: u64,
    /// Sessions touched by the operation.
    #[serde(default)]
    pub sessions: Vec<RestoreSession>,
}

/// One session of a [`RestoreReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RestoreSession {
    /// Identifier of the session.
    pub id: SessionId,
    /// Outcome for the session as a whole.
    pub status: EntryStatus,
    /// Entries of the session, with `planned` status for `restore --list`.
    #[serde(default)]
    pub items: Vec<QuarantineEntry>,
}

/// Payload shared by `broza quarantine expire` and `broza quarantine purge`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ReclaimReport {
    /// [`OperationKind::Expire`] or [`OperationKind::Purge`].
    pub operation: OperationKind,
    /// Bytes actually freed.
    pub reclaimed_bytes: u64,
    /// Sessions touched by the operation.
    #[serde(default)]
    pub sessions: Vec<ReclaimSession>,
}

/// One session of a [`ReclaimReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ReclaimSession {
    /// Identifier of the session.
    pub id: SessionId,
    /// Bytes the session held.
    pub total_bytes: u64,
    /// Number of entries the session held.
    pub item_count: u64,
    /// Outcome for the session.
    pub status: EntryStatus,
}

#[cfg(test)]
mod tests {
    use super::{
        EntryStatus, OperationKind, QuarantineEntry, QuarantineSession, ReclaimReport, ReclaimSession,
        SessionState,
    };
    use crate::model::plan::ItemStatus;

    fn session_json() -> serde_json::Value {
        serde_json::json!({
            "id": "cln_20260917103608_a1b2",
            "created_at": "2026-09-17T10:36:08Z",
            "expires_at": "2026-10-17T10:36:08Z",
            "total_bytes": 138_200_000_000_u64,
            "item_count": 1284,
            "state": "complete"
        })
    }

    #[test]
    fn a_session_without_entries_round_trips_without_adding_fields() {
        let raw = session_json();
        let session: QuarantineSession =
            serde_json::from_value(raw.clone()).unwrap_or_else(|e| panic!("{e}"));
        assert!(session.entries.is_empty());
        assert_eq!(serde_json::to_value(&session).unwrap_or_else(|e| panic!("{e}")), raw);
    }

    #[test]
    fn timestamps_must_be_rfc_3339() {
        let mut raw = session_json();
        raw["created_at"] = serde_json::json!("17/09/2026");
        assert!(serde_json::from_value::<QuarantineSession>(raw).is_err());
    }

    #[test]
    fn session_states_use_the_stable_identifiers() {
        let cases = [
            (SessionState::InProgress, "\"in_progress\""),
            (SessionState::Complete, "\"complete\""),
            (SessionState::Restoring, "\"restoring\""),
            (SessionState::Expired, "\"expired\""),
        ];
        for (state, expected) in cases {
            assert_eq!(serde_json::to_string(&state).unwrap_or_else(|e| panic!("{e}")), expected);
        }
        assert!(serde_json::from_str::<SessionState>("\"vanished\"").is_err());
    }

    #[test]
    fn an_entry_status_is_the_shared_item_status() {
        let status: EntryStatus = ItemStatus::Restored;
        assert_eq!(serde_json::to_string(&status).unwrap_or_else(|e| panic!("{e}")), "\"restored\"");
    }

    #[test]
    fn a_stored_entry_omits_the_paths_it_does_not_have_yet() {
        let entry = QuarantineEntry {
            id: "cln_20260917103608_a1b2/0001".into(),
            original_path: "/Users/x/Library/Caches/example".into(),
            stored_path: None,
            restored_to: None,
            size_bytes: 10,
            status: ItemStatus::Quarantined,
            error: None,
        };
        let json = serde_json::to_value(&entry).unwrap_or_else(|e| panic!("{e}"));
        assert!(json.get("stored_path").is_none());
        assert!(json.get("restored_to").is_none());
        assert!(json.get("error").is_none());
    }

    #[test]
    fn expire_and_purge_share_one_shape() {
        let report = ReclaimReport {
            operation: OperationKind::Expire,
            reclaimed_bytes: 12_400_000_000,
            sessions: vec![ReclaimSession {
                id: "cln_20260801091200_c3d4".parse().unwrap_or_else(|e| panic!("{e}")),
                total_bytes: 12_400_000_000,
                item_count: 37,
                status: ItemStatus::Purged,
            }],
        };
        let json = serde_json::to_value(&report).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(json["operation"], "expire");
        let back: ReclaimReport = serde_json::from_value(json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back, report);
    }
}
