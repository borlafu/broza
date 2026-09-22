//! The items of a [`CleanPlan`](crate::model::CleanPlan) (`docs/cli-spec.md` §4.4).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::finding::Action;
use crate::model::ids::{FindingId, SessionId, VolumeId};
use crate::model::status::{ItemErrorCode, ItemStatus};

/// A quarantine session expired before the plan ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ExpiredSession {
    /// Identifier of the expired session.
    pub id: SessionId,
    /// Bytes freed by deleting it.
    pub freed_bytes: u64,
}

/// One item of a [`CleanPlan`](crate::model::CleanPlan).
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
    /// The snapshot this item deletes, for `tmutil_delete` items only; `path`
    /// is then the mount point of the snapshot's volume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotRef>,
}

/// A local APFS snapshot named by a plan item (`docs/cli-spec.md` §4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SnapshotRef {
    /// The volume the snapshot belongs to; the deletion is scoped to it.
    pub volume: VolumeId,
    /// The snapshot's name, `com.apple.TimeMachine.YYYY-MM-DD-HHMMSS.local`.
    pub name: String,
    /// The snapshot's UUID, which is what the deletion names.
    pub uuid: String,
}

#[cfg(test)]
mod tests {
    use super::CleanItem;
    use crate::model::finding::Action;
    use crate::model::status::ItemStatus;

    #[test]
    fn an_item_without_an_error_omits_the_field() {
        let item = CleanItem {
            path: "/Users/x/Library/Caches/example".into(),
            finding_id: "user-cache.logs".parse().unwrap_or_else(|e| panic!("{e}")),
            size_bytes: 10,
            status: ItemStatus::Planned,
            action: Action::Quarantine,
            error: None,
            snapshot: None,
        };
        let json = serde_json::to_value(&item).unwrap_or_else(|e| panic!("{e}"));
        assert!(json.get("error").is_none());
        assert_eq!(json["status"], "planned");
    }
}
