//! Approving where a restore is allowed to put things back.
//!
//! A quarantine manifest is data on disk. `original_path` is whatever the run
//! that wrote it recorded, and a manifest an attacker can edit would otherwise
//! turn `broza restore` into "write this file anywhere I like". The destinations
//! therefore go through the same checks a clean item does — absolute, no `.` or
//! `..`, no symlinked component, a volume Broza may write to, inside an allowed
//! root — before any of them is renamed into place.
//!
//! The destination normally does **not** exist yet, so the check walks the
//! ancestors it does have: every component that exists must be a real directory,
//! and the first missing one ends the walk.

use std::path::{Path, PathBuf};

use super::token::issue;
use super::{Approved, RestoreWrite};
use crate::model::Action;
use crate::ports::FileOps;
use crate::safety::firmlink::is_volume_root;
use crate::safety::path::reject_relative_components;
use crate::safety::rejection::GuardRejection;
use crate::safety::roles::allows_action;
use crate::safety::roots::{AllowedRoots, RootContext, is_under_allowed_root};
use crate::scan::{MountEntry, MountTable};

/// Everything the guard needs to judge a restore destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreRequest {
    /// The user's home directory (`/Users/<name>`).
    pub home: PathBuf,
    /// Per-uid temporary directories (`/private/var/folders/<xx>/<hash>`).
    pub uid_temp_dirs: Vec<PathBuf>,
    /// `--to`: an alternative directory the user named on the command line.
    ///
    /// Anything strictly inside it is allowed, on any volume Broza may write to,
    /// because the user typed the path. Everything else must be under one of the
    /// standard roots.
    pub to: Option<PathBuf>,
}

impl RestoreRequest {
    /// A request that restores to the original paths under `home`.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into(), uid_temp_dirs: Vec::new(), to: None }
    }
}

/// Approve the destinations a restore will write to.
///
/// # Errors
///
/// The first [`GuardRejection`] any destination produces. A restore refuses as a
/// whole rather than putting half a session in the wrong place.
pub fn approve_restore_targets(
    targets: &[PathBuf],
    request: &RestoreRequest,
    mounts: &MountTable,
    fs: &dyn FileOps,
) -> Result<Approved<RestoreWrite>, GuardRejection> {
    let roots = AllowedRoots::new(&request.home, &request.uid_temp_dirs)?;
    for target in targets {
        check_target(target, request, &roots, mounts, fs)?;
    }
    Ok(issue::<RestoreWrite>(targets.to_vec()))
}

/// Every check one destination has to pass.
fn check_target(
    target: &Path,
    request: &RestoreRequest,
    roots: &AllowedRoots,
    mounts: &MountTable,
    fs: &dyn FileOps,
) -> Result<(), GuardRejection> {
    reject_relative_components(target)?;
    if is_volume_root(target, mounts) {
        return Err(GuardRejection::RootItself(target.to_path_buf()));
    }
    let existing = reject_symlinked_ancestors(target, fs)?;
    let mount =
        mounts.volume_for(&existing).ok_or_else(|| GuardRejection::UnknownVolume(target.to_path_buf()))?;
    allows_action(mount.volume.role, Action::Quarantine).map_err(|rejection| rejection.with_path(target))?;
    if is_allowed_destination(target, request, roots, mount) {
        return Ok(());
    }
    Err(GuardRejection::OutsideAllowedRoots(target.to_path_buf()))
}

/// The deepest ancestor of `target` that exists, having proved none is a symlink.
///
/// A restore recreates the directories the quarantined item used to live in, so
/// the tail of the path is expected to be missing. What must not happen is a
/// *symlink* in the part that does exist: that is how a manifest could aim a
/// rename at a directory nobody approved.
fn reject_symlinked_ancestors(target: &Path, fs: &dyn FileOps) -> Result<PathBuf, GuardRejection> {
    let root = Path::new("/");
    let mut ancestors: Vec<&Path> = target.ancestors().collect();
    ancestors.reverse();
    let mut deepest = root.to_path_buf();
    for ancestor in ancestors {
        let Ok(metadata) = fs.metadata(ancestor) else {
            // The first component that is not there ends the walk: everything
            // below it is missing too, and the restore will create it.
            break;
        };
        if metadata.is_symlink {
            return Err(GuardRejection::SymlinkComponent(ancestor.to_path_buf()));
        }
        deepest = ancestor.to_path_buf();
    }
    Ok(deepest)
}

/// `true` when the destination is somewhere a restore may write.
///
/// The standard allowlist of `docs/cli-spec.md` §3.4 check 4, plus anything
/// strictly inside the `--to` directory the user named. `/Applications` is
/// included because that is where an `unused-apps` item came from and where a
/// restore has to put it back.
fn is_allowed_destination(
    target: &Path,
    request: &RestoreRequest,
    roots: &AllowedRoots,
    mount: &MountEntry,
) -> bool {
    if let Some(to) = request.to.as_ref()
        && target != to
        && target.starts_with(to)
    {
        return true;
    }
    let context = RootContext { category: crate::model::Category::UnusedApps, mount };
    is_under_allowed_root(target, roots, context)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{RestoreRequest, approve_restore_targets};
    use crate::safety::rejection::GuardRejection;
    use crate::testing::{FakeFileOps, mac_mount_table};

    const HOME: &str = "/Users/dana";

    fn fs() -> FakeFileOps {
        FakeFileOps::new()
            .with_root("/", 1)
            .with_root("/Users", 2)
            .with_root("/Volumes/External", 6)
            .with_dir("/Users/dana/Library/Caches")
            .with_dir("/Users/other/Library")
            .with_dir("/Volumes/External/Rescued")
            .with_dir("/System/Library/LaunchDaemons")
    }

    fn approve(targets: &[&str], request: &RestoreRequest) -> Result<(), GuardRejection> {
        let paths: Vec<PathBuf> = targets.iter().map(PathBuf::from).collect();
        approve_restore_targets(&paths, request, &mac_mount_table(), &fs()).map(|_| ())
    }

    #[test]
    fn a_path_inside_the_users_home_is_approved() {
        assert!(approve(&["/Users/dana/Library/Caches/app.cache"], &RestoreRequest::new(HOME)).is_ok());
    }

    #[test]
    fn a_destination_that_does_not_exist_yet_is_approved() {
        let deep = "/Users/dana/Library/Caches/Gone/Deeper/app.cache";

        assert!(approve(&[deep], &RestoreRequest::new(HOME)).is_ok());
    }

    #[test]
    fn another_users_home_is_refused() {
        let evil = "/Users/other/Library/LaunchAgents/evil.plist";

        let rejection = approve(&[evil], &RestoreRequest::new(HOME));

        assert!(matches!(rejection, Err(GuardRejection::OutsideAllowedRoots(_))), "{rejection:?}");
    }

    #[test]
    fn a_protected_volume_is_refused() {
        let rejection = approve(&["/System/Library/LaunchDaemons/evil.plist"], &RestoreRequest::new(HOME));

        assert!(matches!(rejection, Err(GuardRejection::ProtectedVolume { .. })), "{rejection:?}");
    }

    #[test]
    fn a_relative_or_climbing_path_is_refused() {
        assert!(matches!(
            approve(&["Users/dana/a"], &RestoreRequest::new(HOME)),
            Err(GuardRejection::NotAbsolute(_))
        ));
        assert!(matches!(
            approve(&["/Users/dana/../other/a"], &RestoreRequest::new(HOME)),
            Err(GuardRejection::RelativeComponent(_))
        ));
    }

    #[test]
    fn a_symlinked_ancestor_is_refused() {
        let tree = fs();
        tree.add_symlink("/Users/dana/Library/Sneaky", "/Users/other/Library");
        let targets = vec![PathBuf::from("/Users/dana/Library/Sneaky/evil.plist")];

        let rejection =
            approve_restore_targets(&targets, &RestoreRequest::new(HOME), &mac_mount_table(), &tree);

        assert!(matches!(rejection, Err(GuardRejection::SymlinkComponent(_))), "{rejection:?}");
    }

    #[test]
    fn a_volume_root_is_never_a_destination() {
        let rejection = approve(&["/Volumes/External"], &RestoreRequest::new(HOME));

        assert!(matches!(rejection, Err(GuardRejection::RootItself(_))), "{rejection:?}");
    }

    #[test]
    fn an_alternative_directory_opens_the_volume_the_user_named() {
        let to = PathBuf::from("/Volumes/External/Rescued");
        let request = RestoreRequest { to: Some(to), ..RestoreRequest::new(HOME) };

        assert!(approve(&["/Volumes/External/Rescued/0001_app.cache"], &request).is_ok());
        assert!(
            approve(&["/Volumes/External/elsewhere"], &request).is_err(),
            "only what is inside `--to` is opened up"
        );
    }

    #[test]
    fn the_token_carries_every_destination_it_approved() {
        let targets = vec![PathBuf::from("/Users/dana/Library/Caches/a"), PathBuf::from(HOME).join("b")];

        let token = approve_restore_targets(&targets, &RestoreRequest::new(HOME), &mac_mount_table(), &fs())
            .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(token.targets(), targets.as_slice());
        assert!(!token.targets().contains(&PathBuf::from("/Users/other/x")));
    }

    #[test]
    fn a_home_that_is_not_a_home_is_refused_before_any_path_is_looked_at() {
        let rejection = approve(&["/Users/dana/a"], &RestoreRequest::new("/"));

        assert!(matches!(rejection, Err(GuardRejection::InvalidRoot { .. })), "{rejection:?}");
    }
}
