//! Why the safety kernel refuses to write, and how each refusal reaches the user.
//!
//! The kernel has its own error types so that a rejection can be matched on
//! precisely; [`BrozaError`] is the transport to the exit code
//! ([`crate::ExitCode`], `docs/cli-spec.md` §2).
//!
//! # Exit-code rule
//!
//! *Every policy refusal is a usage error* (exit `2`): protected role, symlink
//! component, outside the allowlist, exclusion, `--max-size`, `inform_only`, red,
//! `--yes` with `--purge`, and any internal inconsistency. The user asked for
//! something Broza will not do, and the fix is a different command line.
//!
//! Only a genuine operating-system failure keeps its own code: [`UnreadableCause`]
//! maps `ENOENT` to `TARGET_NOT_FOUND` (`4`) and `EACCES`/`EPERM` to
//! `PERMISSION_DENIED` (`3`). The two confirmation outcomes keep theirs as well:
//! "no" is `6` and "no TTY" is `7`.

use std::fmt;
use std::path::PathBuf;

use crate::BrozaError;
use crate::model::{Action, FindingId, VolumeRole};
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

/// What the operating system said when a path could not be read.
///
/// Classified from the [`BrozaError`] the [`crate::ports::FileOps`] port returned,
/// so that the exit code stays honest about what actually failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreadableCause {
    /// `ENOENT`: nothing at that path (exit `4`).
    NotFound,
    /// `EACCES` or `EPERM`: Full Disk Access is probably missing (exit `3`).
    PermissionDenied,
    /// Anything else (exit `1`).
    Other,
}

impl UnreadableCause {
    /// Classifies the error a [`crate::ports::FileOps`] implementation returned.
    pub fn of(error: &BrozaError) -> Self {
        match error {
            BrozaError::TargetNotFound(_) => Self::NotFound,
            BrozaError::PermissionDenied { .. } => Self::PermissionDenied,
            BrozaError::Io { source, .. } => match source.kind() {
                std::io::ErrorKind::NotFound => Self::NotFound,
                std::io::ErrorKind::PermissionDenied => Self::PermissionDenied,
                _ => Self::Other,
            },
            _ => Self::Other,
        }
    }
}

/// Every reason the safety kernel can refuse to hand out an `Approved` token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuardRejection {
    /// A path that is not absolute can never be checked reliably.
    #[error("path `{0}` is not absolute")]
    NotAbsolute(PathBuf),
    /// The path contains a `.` or `..` component; the guard never resolves those,
    /// because a `..` can jump over a symlink that was already checked.
    #[error("path `{0}` contains a `.` or `..` component")]
    RelativeComponent(PathBuf),
    /// One component of the path is a symbolic link.
    #[error("path component `{0}` is a symbolic link")]
    SymlinkComponent(PathBuf),
    /// A component of the path could not be read; the guard never guesses.
    #[error("cannot read `{path}`: {reason}")]
    Unreadable {
        /// Path that could not be read.
        path: PathBuf,
        /// What the operating system reported.
        cause: UnreadableCause,
        /// The `BrozaError` the filesystem port returned, kept verbatim.
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
    /// An allowed root was configured with a path that is not one.
    #[error("`{path}` is not a usable root: {reason}")]
    InvalidRoot {
        /// The offending root.
        path: PathBuf,
        /// Why it was refused.
        reason: String,
    },
    /// The path matches an exclusion and must not have reached the guard.
    #[error("`{0}` matches an exclusion")]
    Excluded(PathBuf),
    /// An exclusion pattern is not a valid absolute glob.
    #[error("invalid exclusion `{pattern}`: {reason}")]
    InvalidExclusion {
        /// The offending pattern.
        pattern: String,
        /// Why it was refused.
        reason: String,
    },
    /// The plan would remove more than `--max-size`.
    #[error("the plan removes {planned_bytes} bytes, more than the {max_bytes} byte cap")]
    MaxSizeExceeded {
        /// Bytes the plan would actually remove.
        planned_bytes: u64,
        /// Cap given with `--max-size`.
        max_bytes: u64,
    },
    /// The plan contains an item Broza only reports (`cloud-synced`).
    #[error("`{0}` is inform-only; the whole plan is rejected")]
    InformOnlyItem(PathBuf),
    /// The selection contained a finding Broza only reports.
    #[error("finding `{0}` is inform-only and can never be cleaned")]
    InformOnlySelected(FindingId),
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

    /// Builds the rejection for a path the filesystem refused to describe.
    pub fn unreadable(path: &std::path::Path, error: &BrozaError) -> Self {
        Self::Unreadable {
            path: path.to_path_buf(),
            cause: UnreadableCause::of(error),
            reason: error.to_string(),
        }
    }

    /// `true` when the path does not exist, which the guard treats as a skipped
    /// item rather than a refusal (`docs/cli-spec.md` §3.4, cross-volume rule).
    pub fn is_missing_path(&self) -> bool {
        matches!(self, Self::Unreadable { cause: UnreadableCause::NotFound, .. })
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
            GuardRejection::Unreadable { path, cause, .. } => match cause {
                UnreadableCause::NotFound => Self::TargetNotFound(path.display().to_string()),
                UnreadableCause::PermissionDenied => Self::PermissionDenied { path },
                UnreadableCause::Other => Self::Other(message),
            },
            GuardRejection::Policy(error) => error.into(),
            // Every remaining variant is a policy refusal: the command line, not
            // the machine, is what has to change.
            _ => Self::Usage(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GuardRejection, PolicyError, UnreadableCause};
    use crate::model::{Action, VolumeRole};
    use crate::safety::policy::{ConfirmationMode, RejectReason};
    use crate::{BrozaError, ExitCode};

    fn code(rejection: GuardRejection) -> ExitCode {
        ExitCode::from(&BrozaError::from(rejection))
    }

    #[test]
    fn every_policy_refusal_is_a_usage_error() {
        let cases = [
            GuardRejection::NotAbsolute("relative/path".into()),
            GuardRejection::RelativeComponent("/Users/dana/../etc".into()),
            GuardRejection::SymlinkComponent("/Users/dana/link".into()),
            GuardRejection::UnknownVolume("/nowhere".into()),
            GuardRejection::ProtectedVolume { path: "/System/Library".into(), role: VolumeRole::System },
            GuardRejection::ActionNotAllowed {
                path: "/Volumes/Backup/x".into(),
                role: VolumeRole::Backup,
                action: Action::Quarantine,
            },
            GuardRejection::OutsideAllowedRoots("/etc/passwd".into()),
            GuardRejection::RootItself("/Users/dana".into()),
            GuardRejection::InvalidRoot { path: "/".into(), reason: "too broad".into() },
            GuardRejection::Excluded("/Users/dana/keep".into()),
            GuardRejection::InvalidExclusion { pattern: "[".into(), reason: "unclosed".into() },
            GuardRejection::MaxSizeExceeded { planned_bytes: 10, max_bytes: 1 },
            GuardRejection::InformOnlyItem("/Users/dana/iCloud".into()),
            GuardRejection::InformOnlySelected(
                "cloud-synced.icloud".parse().unwrap_or_else(|e| panic!("{e}")),
            ),
            GuardRejection::OutsideQuarantineStore {
                path: "/Users/dana/elsewhere".into(),
                store_root: "/Users/dana/.local/share/broza/quarantine".into(),
            },
            GuardRejection::Policy(PolicyError::Rejected(RejectReason::RedNotActionable)),
            GuardRejection::Policy(PolicyError::Rejected(RejectReason::YesWithPurge)),
            GuardRejection::Inconsistent("bug".into()),
        ];
        for case in cases {
            assert_eq!(code(case.clone()), ExitCode::UsageError, "{case}");
        }
    }

    #[test]
    fn only_the_operating_system_can_produce_three_and_four() {
        let missing = GuardRejection::unreadable(
            std::path::Path::new("/Users/dana/gone"),
            &BrozaError::TargetNotFound("/Users/dana/gone".into()),
        );
        assert!(missing.is_missing_path());
        assert_eq!(code(missing), ExitCode::TargetNotFound);

        let denied = GuardRejection::unreadable(
            std::path::Path::new("/Users/dana/secret"),
            &BrozaError::PermissionDenied { path: "/Users/dana/secret".into() },
        );
        assert!(!denied.is_missing_path());
        assert_eq!(code(denied), ExitCode::PermissionDenied);

        let broken = GuardRejection::unreadable(
            std::path::Path::new("/Users/dana/x"),
            &BrozaError::Io {
                context: "lstat".into(),
                source: std::io::Error::from(std::io::ErrorKind::InvalidData),
            },
        );
        assert_eq!(code(broken), ExitCode::GenericError);
    }

    #[test]
    fn io_errors_are_classified_by_their_kind() {
        let cases = [
            (std::io::ErrorKind::NotFound, UnreadableCause::NotFound),
            (std::io::ErrorKind::PermissionDenied, UnreadableCause::PermissionDenied),
            (std::io::ErrorKind::Other, UnreadableCause::Other),
        ];
        for (kind, expected) in cases {
            let error = BrozaError::Io { context: "lstat".into(), source: std::io::Error::from(kind) };
            assert_eq!(UnreadableCause::of(&error), expected, "{kind:?}");
        }
        assert_eq!(UnreadableCause::of(&BrozaError::Usage("x".into())), UnreadableCause::Other);
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
