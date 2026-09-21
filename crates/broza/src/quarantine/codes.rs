//! The two item error codes the store needs and `docs/cli-spec.md` §4.1 does not
//! list yet.
//!
//! [`ItemErrorCode`] is an *open* enum precisely so a token this version does not
//! know survives a read-modify-write cycle (`docs/cli-spec.md` §4.1). Both codes
//! below ride that mechanism: consumers must ignore an unknown value, so emitting
//! them is forward-compatible, and adding them to the table in §4.1 is a purely
//! additive spec change that belongs in the milestone that touches the spec.
//!
//! Neither reuses an existing token, because both describe something a reader has
//! to act on differently: `not_found` means "it was gone", while
//! [`CHANGED_SINCE_CHECK`] means "something else is there now", and
//! [`MAX_SIZE_EXCEEDED`] is a policy stop, not an I/O failure.

use crate::model::ItemErrorCode;

/// Token of [`changed_since_check`].
pub const CHANGED_SINCE_CHECK: &str = "changed_since_check";
/// Token of [`max_size_exceeded`].
pub const MAX_SIZE_EXCEEDED: &str = "max_size_exceeded";

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

#[cfg(test)]
mod tests {
    use super::{CHANGED_SINCE_CHECK, MAX_SIZE_EXCEEDED, changed_since_check, max_size_exceeded};

    #[test]
    fn both_codes_serialise_as_their_token() {
        for (code, token) in
            [(changed_since_check(), CHANGED_SINCE_CHECK), (max_size_exceeded(), MAX_SIZE_EXCEEDED)]
        {
            assert_eq!(code.to_string(), token);
            assert_eq!(serde_json::to_string(&code).unwrap_or_default(), format!("\"{token}\""));
            assert!(!code.is_known(), "the specification does not list this token yet");
        }
    }
}
