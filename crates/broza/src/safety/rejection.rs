//! Why the safety kernel refuses to write, and how each refusal reaches the user.
//!
//! The kernel has its own error types so that a rejection can be matched on
//! precisely; [`BrozaError`] is the transport to the exit code
//! ([`crate::ExitCode`], `docs/cli-spec.md` §2):
//!
//! | Rejection kind | `BrozaError` | Exit |
//! |---|---|---|
//! | usage-shaped (relative path, exclusion, `--max-size`, `inform_only`, red, `--yes --purge`) | `Usage` | `2` |
//! | path-shaped (symlink, protected role, outside the allowlist, outside the store) | `PermissionDenied` | `3` |
//! | no TTY and no `--yes` | `ConfirmationRequired` | `7` |
//! | the user answered "no" | `AbortedByUser` | `6` |
//! | internal inconsistency | `Other` | `1` |

use std::fmt;
use std::path::PathBuf;

use crate::BrozaError;
use crate::model::{Action, VolumeRole};
use crate::safety::policy::{ConfirmationMode, RejectReason};
use crate::safety::roles::role_id;

/// Outcome of the confirmation step that stops the operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// The plan is refused outright (exit `2`).
    #[error("{0}")]
    Rejected(RejectReason),
    /// Confirmation was required and no interactive terminal was available (exit `7`).
    #[error("confirmation required: no TTY and --yes not given")]
    ConfirmationRequired,
    /// The user declined the confirmation (exit `6`).
    #[error("aborted by user")]
    AbortedByUser,
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl TryFrom<ConfirmationMode> for PolicyError {
    type Error = ConfirmationMode;

    /// Converts the two modes that stop the operation; any other mode is returned
    /// unchanged in `Err`, because it is not an error at all.
    fn try_from(mode: ConfirmationMode) -> Result<Self, Self::Error> {
        match mode {
            ConfirmationMode::Rejected(reason) => Ok(Self::Rejected(reason)),
            ConfirmationMode::RequiredButNoTty => Ok(Self::ConfirmationRequired),
            other => Err(other),
        }
    }
}

/// Every reason the safety kernel can refuse to hand out an `Approved` token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuardRejection {
    /// A path that is not absolute can never be checked reliably.
    #[error("path `{0}` is not absolute")]
    NotAbsolute(PathBuf),
    /// `..` climbed above the filesystem root.
    #[error("path `{0}` escapes the filesystem root")]
    EscapesRoot(PathBuf),
    /// One component of the path is a symbolic link.
    #[error("path component `{0}` is a symbolic link")]
    SymlinkComponent(PathBuf),
    /// A component of the path could not be read; the guard never guesses.
    #[error("cannot read `{path}`: {reason}")]
    Unreadable {
        /// Path that could not be read.
        path: PathBuf,
        /// Why the filesystem refused.
        reason: String,
    },
    /// No mounted volume contains the path.
    #[error("no mounted volume contains `{0}`")]
    UnknownVolume(PathBuf),
    /// The volume has a role Broza never writes to (`AGENTS.md` §2.3).
    #[error("`{path}` is on a protected volume (role `{role}`)", role = role_id(*role))]
    ProtectedVolume {
        /// Path that was refused; empty until the guard fills it in with [`GuardRejection::with_path`].
        path: PathBuf,
        /// Role of the volume it resolves to.
        role: VolumeRole,
    },
    /// The action is not allowed on a volume with this role.
    #[error("`{path}`: action `{action:?}` is not allowed on a volume with role `{role}`", role = role_id(*role))]
    ActionNotAllowed {
        /// Path that was refused; empty until the guard fills it in with [`GuardRejection::with_path`].
        path: PathBuf,
        /// Role of the volume.
        role: VolumeRole,
        /// Action that was requested.
        action: Action,
    },
    /// The path is not under any allowed root (`docs/cli-spec.md` §3.4, check 4).
    #[error("`{0}` is not under an allowed root")]
    OutsideAllowedRoots(PathBuf),
    /// The path *is* an allowed root; Broza only ever touches things inside one.
    #[error("`{0}` is an allowed root itself and is never removed")]
    RootItself(PathBuf),
    /// The path matches an exclusion and must not have reached the guard.
    #[error("`{0}` matches an exclusion")]
    Excluded(PathBuf),
    /// An exclusion pattern is not a valid glob.
    #[error("invalid exclusion `{pattern}`: {reason}")]
    InvalidExclusion {
        /// The offending pattern.
        pattern: String,
        /// Why `globset` refused it.
        reason: String,
    },
    /// The plan would remove more than `--max-size`.
    #[error("the plan removes {planned_bytes} bytes, more than the {max_bytes} byte cap")]
    MaxSizeExceeded {
        /// Bytes the plan would remove.
        planned_bytes: u64,
        /// Cap given with `--max-size`.
        max_bytes: u64,
    },
    /// The plan contains an item Broza only reports (`cloud-synced`).
    #[error("`{0}` is inform-only; the whole plan is rejected")]
    InformOnlyItem(PathBuf),
    /// A quarantine operation addressed something outside the quarantine store.
    #[error("`{path}` is outside the quarantine store `{store_root}`")]
    OutsideQuarantineStore {
        /// Path that was refused.
        path: PathBuf,
        /// Root of the quarantine store.
        store_root: PathBuf,
    },
    /// The confirmation step stopped the operation.
    #[error("{0}")]
    Policy(#[from] PolicyError),
    /// The guard was handed something it cannot reason about (a bug in the caller).
    #[error("{0}")]
    Inconsistent(String),
}

impl GuardRejection {
    /// Fills in the path of a role-based rejection produced without one.
    ///
    /// [`crate::safety::roles::allows_action`] only knows the role and the action;
    /// the guard, which knows the path, completes the message.
    #[must_use]
    pub fn with_path(self, path: &std::path::Path) -> Self {
        match self {
            Self::ProtectedVolume { role, .. } => Self::ProtectedVolume { path: path.to_path_buf(), role },
            Self::ActionNotAllowed { role, action, .. } => {
                Self::ActionNotAllowed { path: path.to_path_buf(), role, action }
            }
            other => other,
        }
    }
}

impl From<PolicyError> for BrozaError {
    fn from(error: PolicyError) -> Self {
        match error {
            PolicyError::Rejected(reason) => Self::Usage(reason.message().to_owned()),
            PolicyError::ConfirmationRequired => Self::ConfirmationRequired,
            PolicyError::AbortedByUser => Self::AbortedByUser,
        }
    }
}

impl From<GuardRejection> for BrozaError {
    fn from(rejection: GuardRejection) -> Self {
        let message = rejection.to_string();
        match rejection {
            GuardRejection::SymlinkComponent(path)
            | GuardRejection::Unreadable { path, .. }
            | GuardRejection::UnknownVolume(path)
            | GuardRejection::ProtectedVolume { path, .. }
            | GuardRejection::OutsideAllowedRoots(path)
            | GuardRejection::RootItself(path)
            | GuardRejection::ActionNotAllowed { path, .. }
            | GuardRejection::OutsideQuarantineStore { path, .. } => Self::PermissionDenied { path },
            GuardRejection::NotAbsolute(_)
            | GuardRejection::EscapesRoot(_)
            | GuardRejection::Excluded(_)
            | GuardRejection::InvalidExclusion { .. }
            | GuardRejection::MaxSizeExceeded { .. }
            | GuardRejection::InformOnlyItem(_) => Self::Usage(message),
            GuardRejection::Policy(error) => error.into(),
            GuardRejection::Inconsistent(_) => Self::Other(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GuardRejection, PolicyError};
    use crate::model::{Action, VolumeRole};
    use crate::safety::policy::{ConfirmationMode, RejectReason};
    use crate::{BrozaError, ExitCode};

    fn code(rejection: GuardRejection) -> ExitCode {
        ExitCode::from(&BrozaError::from(rejection))
    }

    #[test]
    fn usage_shaped_rejections_exit_with_two() {
        let cases = [
            GuardRejection::NotAbsolute("relative/path".into()),
            GuardRejection::EscapesRoot("/../..".into()),
            GuardRejection::Excluded("/Users/dana/keep".into()),
            GuardRejection::InvalidExclusion { pattern: "[".into(), reason: "unclosed".into() },
            GuardRejection::MaxSizeExceeded { planned_bytes: 10, max_bytes: 1 },
            GuardRejection::InformOnlyItem("/Users/dana/iCloud".into()),
            GuardRejection::Policy(PolicyError::Rejected(RejectReason::RedNotActionable)),
            GuardRejection::Policy(PolicyError::Rejected(RejectReason::YesWithPurge)),
        ];
        for case in cases {
            assert_eq!(code(case.clone()), ExitCode::UsageError, "{case}");
        }
    }

    #[test]
    fn path_shaped_rejections_exit_with_three() {
        let cases = [
            GuardRejection::SymlinkComponent("/Users/dana/link".into()),
            GuardRejection::Unreadable { path: "/Users/dana/gone".into(), reason: "missing".into() },
            GuardRejection::UnknownVolume("/nowhere".into()),
            GuardRejection::ProtectedVolume { path: "/System/Library".into(), role: VolumeRole::System },
            GuardRejection::ActionNotAllowed {
                path: "/Volumes/Backup/x".into(),
                role: VolumeRole::Backup,
                action: Action::Quarantine,
            },
            GuardRejection::OutsideAllowedRoots("/etc/passwd".into()),
            GuardRejection::RootItself("/Users/dana".into()),
            GuardRejection::OutsideQuarantineStore {
                path: "/Users/dana/elsewhere".into(),
                store_root: "/Users/dana/.local/share/broza/quarantine".into(),
            },
        ];
        for case in cases {
            assert_eq!(code(case.clone()), ExitCode::PermissionDenied, "{case}");
        }
    }

    #[test]
    fn the_confirmation_outcomes_map_to_seven_and_six() {
        assert_eq!(
            code(GuardRejection::Policy(PolicyError::ConfirmationRequired)),
            ExitCode::ConfirmationRequired
        );
        assert_eq!(code(GuardRejection::Policy(PolicyError::AbortedByUser)), ExitCode::AbortedByUser);
    }

    #[test]
    fn an_internal_inconsistency_is_a_generic_error() {
        assert_eq!(code(GuardRejection::Inconsistent("bug".into())), ExitCode::GenericError);
    }

    #[test]
    fn only_the_two_stopping_modes_become_policy_errors() {
        assert_eq!(
            PolicyError::try_from(ConfirmationMode::Rejected(RejectReason::YesWithPurge)),
            Ok(PolicyError::Rejected(RejectReason::YesWithPurge))
        );
        assert_eq!(
            PolicyError::try_from(ConfirmationMode::RequiredButNoTty),
            Ok(PolicyError::ConfirmationRequired)
        );
        assert_eq!(PolicyError::try_from(ConfirmationMode::SimpleYesNo), Err(ConfirmationMode::SimpleYesNo));
    }

    #[test]
    fn rejections_explain_themselves() {
        let rejection =
            GuardRejection::ProtectedVolume { path: "/System/Library".into(), role: VolumeRole::System };
        assert_eq!(rejection.to_string(), "`/System/Library` is on a protected volume (role `system`)");
        assert_eq!(GuardRejection::Policy(PolicyError::AbortedByUser).to_string(), "aborted by user");
    }
}
