//! The status and error tokens the store writes that [`ItemStatus`] and
//! [`ItemErrorCode`] do not have variants for.
//!
//! Both are *open* enums precisely so a token one version does not know survives
//! a read-modify-write cycle (`docs/cli-spec.md` §4.1), and all three tokens
//! below ride that mechanism. The model keeps its variants in step with the
//! specification; this module is where the store names the values it needs
//! without reaching into `model/`.
//!
//! None of them reuses an existing token, because each means something a reader
//! has to act on differently: `not_found` means "it was gone", while
//! [`CHANGED_SINCE_CHECK`] means "something else is there now";
//! [`MAX_SIZE_EXCEEDED`] is a policy stop, not an I/O failure; and [`MOVING`]
//! marks the window in which an item may be in either place.

use crate::model::{ItemErrorCode, ItemStatus};

/// Token of [`changed_since_check`].
pub const CHANGED_SINCE_CHECK: &str = "changed_since_check";
/// Token of [`max_size_exceeded`].
pub const MAX_SIZE_EXCEEDED: &str = "max_size_exceeded";
/// Token of [`moving`].
pub const MOVING: &str = "moving";

/// The `(device, inode)` of the path is not the pair the guard approved.
///
/// The path was renamed, replaced or recreated between the check and the write,
/// so acting on it would act on something nobody approved (`AGENTS.md` §4).
pub fn changed_since_check() -> ItemErrorCode {
    ItemErrorCode::from_token(CHANGED_SINCE_CHECK)
}

/// Moving the item would have taken the run past `--max-size`.
///
/// The cap is re-checked against the *measured* size of a directory immediately
/// before the move (`docs/cli-spec.md` §3.4, check 6), so an item the scan
/// under-estimated is abandoned here rather than moved.
pub fn max_size_exceeded() -> ItemErrorCode {
    ItemErrorCode::from_token(MAX_SIZE_EXCEEDED)
}

/// The item is between its original path and the store.
///
/// Written to the manifest *before* the rename and replaced by `quarantined`
/// after it, so a run that dies in between leaves evidence that something may
/// have moved. Only ever appears inside a `manifest.json`; reading a session
/// settles it against the filesystem
/// ([`mod@crate::quarantine::reconcile`]), so no command ever reports it.
pub fn moving() -> ItemStatus {
    ItemStatus::from_token(MOVING)
}

#[cfg(test)]
mod tests {
    use super::{
        CHANGED_SINCE_CHECK, MAX_SIZE_EXCEEDED, MOVING, changed_since_check, max_size_exceeded, moving,
    };

    #[test]
    fn both_error_codes_serialise_as_their_token() {
        for (code, token) in
            [(changed_since_check(), CHANGED_SINCE_CHECK), (max_size_exceeded(), MAX_SIZE_EXCEEDED)]
        {
            assert_eq!(code.to_string(), token);
            assert_eq!(serde_json::to_string(&code).unwrap_or_default(), format!("\"{token}\""));
        }
    }

    #[test]
    fn the_moving_status_serialises_as_its_token_and_is_its_own_value() {
        assert_eq!(moving().to_string(), MOVING);
        assert_eq!(serde_json::to_string(&moving()).unwrap_or_default(), "\"moving\"");
        assert_ne!(moving(), crate::model::ItemStatus::Quarantined);
        assert!(!moving().is_unsuccessful(), "an item in flight has not failed");
    }
}
