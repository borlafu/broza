//! From the `Roles[]` array of `diskutil apfs list` to [`VolumeRole`].
//!
//! The mapping is deliberately narrow. A role decides whether Broza may ever
//! write to a volume (`AGENTS.md` §2.3), so anything this module does not
//! recognise becomes [`VolumeRole::Unknown`], which is not writable. Adding a
//! token here is a safety decision, not a formatting one.

use std::path::Path;

use crate::model::VolumeRole;

/// Directory under which macOS mounts volumes it did not create itself.
const USER_VOLUMES_ROOT: &str = "/Volumes";

/// Map the `Roles[]` array of an APFS volume to a [`VolumeRole`].
///
/// The first token Broza recognises wins; a volume with several roles is rare
/// and `diskutil` lists the defining one first. Tokens are matched exactly, as
/// `diskutil` writes them: a token in unexpected casing is a token this version
/// does not know, and the safe reading of that is `unknown`.
///
/// `Update`, `Hardware` and `xART` are recognised as *not* interesting on
/// purpose: they are real macOS roles that Broza must never write to and has
/// nothing to say about, so they collapse into `unknown` like anything else.
///
/// An empty array is `unknown` here; [`volume_role`] is the function that knows
/// an unmounted-role volume under `/Volumes` is a plain user volume.
pub fn roles_to_volume_role(roles: &[String]) -> VolumeRole {
    roles.iter().find_map(|role| known_role(role)).unwrap_or(VolumeRole::Unknown)
}

/// Role of an APFS volume, given its roles and where it is mounted.
///
/// A volume with no role at all is what `diskutil` reports for a plain APFS
/// volume the user created: an external disk, a second volume on the internal
/// one. Mounted under `/Volumes`, that is [`VolumeRole::User`]; anywhere else
/// Broza does not know what it is looking at and says so.
pub fn volume_role(roles: &[String], mount_point: Option<&Path>) -> VolumeRole {
    let role = roles_to_volume_role(roles);
    if role != VolumeRole::Unknown || !roles.is_empty() {
        return role;
    }
    if mount_point.is_some_and(is_user_volume_mount_point) { VolumeRole::User } else { VolumeRole::Unknown }
}

/// The role of one token, or `None` when Broza does not model it.
fn known_role(role: &str) -> Option<VolumeRole> {
    match role {
        "System" => Some(VolumeRole::System),
        "Data" => Some(VolumeRole::Data),
        "Preboot" => Some(VolumeRole::Preboot),
        "Recovery" => Some(VolumeRole::Recovery),
        "VM" => Some(VolumeRole::Vm),
        "Backup" => Some(VolumeRole::Backup),
        _ => None,
    }
}

/// `true` for a path macOS mounts a user volume at, and not for `/Volumes` itself.
fn is_user_volume_mount_point(mount_point: &Path) -> bool {
    let root = Path::new(USER_VOLUMES_ROOT);
    mount_point != root && mount_point.starts_with(root)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{roles_to_volume_role, volume_role};
    use crate::model::VolumeRole;

    fn roles(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|token| (*token).to_owned()).collect()
    }

    #[test]
    fn every_token_broza_models_maps_to_its_role() {
        let cases = [
            (vec!["System"], VolumeRole::System),
            (vec!["Data"], VolumeRole::Data),
            (vec!["Preboot"], VolumeRole::Preboot),
            (vec!["Recovery"], VolumeRole::Recovery),
            (vec!["VM"], VolumeRole::Vm),
            (vec!["Backup"], VolumeRole::Backup),
        ];
        for (tokens, expected) in cases {
            assert_eq!(roles_to_volume_role(&roles(&tokens)), expected, "{tokens:?}");
        }
    }

    #[test]
    fn every_token_broza_does_not_model_is_unknown_and_not_writable() {
        let unknown = [
            vec!["Update"],
            vec!["Hardware"],
            vec!["xART"],
            vec!["Enterprise"],
            vec!["Sidecar"],
            vec!["system"],
            vec!["DATA"],
            vec![""],
            vec![],
        ];
        for tokens in unknown {
            let role = roles_to_volume_role(&roles(&tokens));
            assert_eq!(role, VolumeRole::Unknown, "{tokens:?}");
            assert!(!role.writable_by_broza(), "{tokens:?}");
        }
    }

    #[test]
    fn the_first_recognised_role_of_several_wins() {
        assert_eq!(roles_to_volume_role(&roles(&["Data", "System"])), VolumeRole::Data);
        assert_eq!(roles_to_volume_role(&roles(&["Update", "System"])), VolumeRole::System);
    }

    #[test]
    fn a_volume_with_no_role_mounted_under_volumes_is_a_user_volume() {
        let mount = Path::new("/Volumes/Backup Disk");

        assert_eq!(volume_role(&[], Some(mount)), VolumeRole::User);
        assert!(volume_role(&[], Some(mount)).writable_by_broza());
    }

    #[test]
    fn a_volume_with_no_role_anywhere_else_stays_unknown() {
        let cases = [
            None,
            Some(Path::new("/")),
            Some(Path::new("/Volumes")),
            Some(Path::new("/System/Volumes/Data")),
            Some(Path::new("/private/var/folders/aa/bb/T/mounted")),
        ];
        for mount_point in cases {
            assert_eq!(volume_role(&[], mount_point), VolumeRole::Unknown, "{mount_point:?}");
        }
    }

    #[test]
    fn a_named_role_is_never_overridden_by_the_mount_point() {
        let mount = Path::new("/Volumes/Something");

        assert_eq!(volume_role(&roles(&["System"]), Some(mount)), VolumeRole::System);
        assert_eq!(
            volume_role(&roles(&["Update"]), Some(mount)),
            VolumeRole::Unknown,
            "a role Broza does not model must not become a writable user volume"
        );
    }
}
