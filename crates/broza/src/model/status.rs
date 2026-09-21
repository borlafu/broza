//! Item status and error codes shared by `clean`, `restore` and the quarantine store.
//!
//! Both are *open* enums: the values are persisted in `manifest.json`, so a token
//! written by a newer Broza is preserved verbatim instead of rejected (see
//! [`crate::model`]).

use crate::model::open_enum::open_enum;

/// Outcome of a clean, restore or quarantine item (`docs/cli-spec.md` §4.1, `status`).
///
/// An open enum: statuses are persisted in the quarantine manifest, so a value from a
/// newer Broza is preserved instead of rejected.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    /// A status this version of Broza does not know, with its original token.
    Unknown(String),
}

open_enum!(ItemStatus {
    Planned => "planned",
    Quarantined => "quarantined",
    Purged => "purged",
    Restored => "restored",
    Skipped => "skipped",
    Failed => "failed",
});

impl ItemStatus {
    /// `true` for the statuses that carry an [`ItemErrorCode`]: `skipped` and `failed`.
    pub fn is_unsuccessful(&self) -> bool {
        matches!(self, Self::Skipped | Self::Failed)
    }
}

/// Item-level error code (`docs/cli-spec.md` §4.1, `error`).
///
/// An open enum, for the same reason as [`ItemStatus`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    /// A code this version of Broza does not know, with its original token.
    Unknown(String),
}

open_enum!(ItemErrorCode {
    CrossVolume => "cross_volume",
    PermissionDenied => "permission_denied",
    Collision => "collision",
    NotFound => "not_found",
    ProtectedVolume => "protected_volume",
    SessionBusy => "session_busy",
    IoError => "io_error",
});

#[cfg(test)]
mod tests {
    use super::{ItemErrorCode, ItemStatus};

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
            assert!(status.is_known());
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
            assert!(code.is_known());
        }
    }

    #[test]
    fn a_status_or_code_from_a_newer_broza_survives_a_round_trip() {
        let status: ItemStatus = serde_json::from_str("\"teleported\"").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(status, ItemStatus::Unknown("teleported".into()));
        assert!(!status.is_known());
        assert!(!status.is_unsuccessful());
        assert_eq!(serde_json::to_string(&status).unwrap_or_else(|e| panic!("{e}")), "\"teleported\"");

        let code: ItemErrorCode = serde_json::from_str("\"meteorite\"").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(code.to_string(), "meteorite");
        assert_eq!(serde_json::to_string(&code).unwrap_or_else(|e| panic!("{e}")), "\"meteorite\"");
    }
}
