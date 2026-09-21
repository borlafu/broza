//! Volume roles: which volumes Broza may write to, and for which action.
//!
//! `AGENTS.md` §2.3 and `docs/cli-spec.md` §0.7: `system`, `preboot`, `recovery` and
//! `vm` are read-only for Broza. No flag changes this, and none will ever be added.

use std::path::PathBuf;

use crate::model::{Action, VolumeRole};
use crate::safety::rejection::GuardRejection;

/// Roles Broza never writes to, in the order the specification lists them.
const PROTECTED_ROLES: [VolumeRole; 4] =
    [VolumeRole::System, VolumeRole::Preboot, VolumeRole::Recovery, VolumeRole::Vm];

/// Stable identifier of a role, as used in the JSON contract and in messages.
pub const fn role_id(role: VolumeRole) -> &'static str {
    match role {
        VolumeRole::System => "system",
        VolumeRole::Data => "data",
        VolumeRole::Preboot => "preboot",
        VolumeRole::Recovery => "recovery",
        VolumeRole::Vm => "vm",
        VolumeRole::Backup => "backup",
        VolumeRole::User => "user",
        _ => "unknown",
    }
}

/// `true` for the four roles that are read-only for Broza with no exception.
pub fn is_protected(role: VolumeRole) -> bool {
    PROTECTED_ROLES.contains(&role)
}

/// Checks that `action` may be performed on a volume with this `role`.
///
/// - protected roles refuse everything;
/// - `backup` only accepts [`Action::TmutilDelete`] (Time Machine's own tool);
/// - an unrecognised role is treated as the most restrictive one and refuses
///   everything, so a future macOS role can never become writable by accident;
/// - [`Action::InformOnly`] is not a write at all and is always refused here; the
///   guard rejects the whole plan for it (`docs/cli-spec.md` §3.4, check 7).
///
/// The returned rejection carries no path; the guard adds it with
/// [`GuardRejection::with_path`].
pub fn allows_action(role: VolumeRole, action: Action) -> Result<(), GuardRejection> {
    if is_protected(role) {
        return Err(GuardRejection::ProtectedVolume { path: PathBuf::new(), role });
    }
    let refused = || Err(GuardRejection::ActionNotAllowed { path: PathBuf::new(), role, action });
    if action == Action::InformOnly {
        return refused();
    }
    match role {
        VolumeRole::Data | VolumeRole::User => Ok(()),
        VolumeRole::Backup if action == Action::TmutilDelete => Ok(()),
        _ => refused(),
    }
}

#[cfg(test)]
mod tests {
    use super::{allows_action, is_protected, role_id};
    use crate::model::{Action, VolumeRole};
    use crate::safety::rejection::GuardRejection;

    const ALL_ROLES: [VolumeRole; 8] = [
        VolumeRole::System,
        VolumeRole::Data,
        VolumeRole::Preboot,
        VolumeRole::Recovery,
        VolumeRole::Vm,
        VolumeRole::Backup,
        VolumeRole::User,
        VolumeRole::Unknown,
    ];

    const WRITE_ACTIONS: [Action; 3] = [Action::Quarantine, Action::Purge, Action::TmutilDelete];

    #[test]
    fn exactly_four_roles_are_protected() {
        for role in ALL_ROLES {
            let expected = matches!(
                role,
                VolumeRole::System | VolumeRole::Preboot | VolumeRole::Recovery | VolumeRole::Vm
            );
            assert_eq!(is_protected(role), expected, "{role:?}");
        }
    }

    #[test]
    fn protected_roles_refuse_every_action() {
        for role in ALL_ROLES.into_iter().filter(|role| is_protected(*role)) {
            for action in WRITE_ACTIONS {
                assert_eq!(
                    allows_action(role, action),
                    Err(GuardRejection::ProtectedVolume { path: std::path::PathBuf::new(), role }),
                    "{role:?} {action:?}"
                );
            }
        }
    }

    #[test]
    fn data_and_user_volumes_accept_every_write_action() {
        for role in [VolumeRole::Data, VolumeRole::User] {
            for action in WRITE_ACTIONS {
                assert_eq!(allows_action(role, action), Ok(()), "{role:?} {action:?}");
            }
        }
    }

    #[test]
    fn a_backup_volume_only_accepts_tmutil_delete() {
        assert_eq!(allows_action(VolumeRole::Backup, Action::TmutilDelete), Ok(()));
        for action in [Action::Quarantine, Action::Purge] {
            assert_eq!(
                allows_action(VolumeRole::Backup, action),
                Err(GuardRejection::ActionNotAllowed {
                    path: std::path::PathBuf::new(),
                    role: VolumeRole::Backup,
                    action,
                })
            );
        }
    }

    #[test]
    fn an_unknown_role_is_never_writable() {
        for action in WRITE_ACTIONS {
            assert!(allows_action(VolumeRole::Unknown, action).is_err(), "{action:?}");
        }
    }

    #[test]
    fn inform_only_is_never_a_write() {
        for role in ALL_ROLES {
            assert!(allows_action(role, Action::InformOnly).is_err(), "{role:?}");
        }
    }

    #[test]
    fn role_identifiers_match_the_json_contract() {
        for role in ALL_ROLES {
            let json = serde_json::to_string(&role).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(json, format!("\"{}\"", role_id(role)));
        }
    }

    #[test]
    fn the_rejection_gains_the_path_at_the_guard() {
        let rejection = allows_action(VolumeRole::System, Action::Purge)
            .err()
            .unwrap_or_else(|| panic!("a protected role must refuse"));
        let located = rejection.with_path(std::path::Path::new("/System/Library"));
        assert_eq!(
            located,
            GuardRejection::ProtectedVolume { path: "/System/Library".into(), role: VolumeRole::System }
        );
    }
}
