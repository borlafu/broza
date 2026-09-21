//! Deciding the role of a volume.
//!
//! A role decides whether Broza may ever write to a volume (`AGENTS.md` §2.3),
//! so this module is written to answer "no" whenever it is not certain of the
//! answer. Three rules follow from that and none of them is a formatting
//! choice:
//!
//! * a protected token wins over a writable one, so a volume that is both
//!   `Data` and `System` is read-only;
//! * a token this version does not model is [`VolumeRole::Unknown`], never a
//!   guess at the closest one;
//! * a volume with no role at all is [`VolumeRole::User`] only when macOS says
//!   it is a writable volume mounted directly under `/Volumes`, and it is not a
//!   backup destination.

use std::path::Path;

use crate::model::VolumeRole;

/// Directory under which macOS mounts volumes it did not create itself.
const USER_VOLUMES_ROOT: &str = "/Volumes";
/// Directory a Time Machine destination keeps its backups in, `HFS+` style.
pub const TIME_MACHINE_MARKER: &str = "Backups.backupdb";
/// Fragments that only appear in the naming of a Time Machine destination.
const TIME_MACHINE_HINTS: [&str; 3] = ["com.apple.TimeMachine", "Time Machine", "TimeMachine"];

/// What `tmutil destinationinfo` says about one volume, and how firmly.
///
/// The distinction is the difference between "this volume is a destination"
/// and "a destination happens to share this name", and it decides whether the
/// answer may override the role macOS declared
/// ([`crate::adapters::tmutil_destinations`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BackupEvidence {
    /// Time Machine said nothing about this volume.
    #[default]
    None,
    /// A destination goes by this volume's name, and nothing stronger.
    ///
    /// A name belongs to no volume in particular — a network share has one
    /// and no disk behind it — so this only counts for a volume macOS gave no
    /// role at all, where the alternative reading is "the user's disk".
    Name,
    /// Time Machine named this very volume, by mount point or by identifier.
    ///
    /// A fact about the volume itself, and therefore strong enough to outrank
    /// the declared role.
    Volume,
}

/// What the adapters could observe about a volume besides its roles.
///
/// Everything here defaults to the cautious answer: not writable, no backup
/// marker, nothing known about the content or the name.
#[derive(Debug, Clone, Copy, Default)]
pub struct VolumeFacts<'a> {
    /// `true` only when `diskutil info` reports `WritableVolume`.
    pub writable_volume: bool,
    /// How strongly Time Machine claims this volume as a destination.
    pub backup_evidence: BackupEvidence,
    /// `true` when a Time Machine backup directory sits at the volume root.
    pub time_machine_marker: bool,
    /// Partition content token (`Apple_HFS`, `Apple_APFS`), when known.
    pub content: Option<&'a str>,
    /// Volume name as Finder shows it.
    pub name: Option<&'a str>,
}

/// Map the `Roles[]` array of an APFS volume to a [`VolumeRole`].
///
/// A protected role wins over a writable one: `["Data", "System"]` is a system
/// volume, because the question this answers is "may Broza write here", and the
/// answer for anything carrying `System` is no.
///
/// Tokens are matched exactly, as `diskutil` writes them: a token in unexpected
/// casing is a token this version does not know, and the safe reading of that
/// is `unknown`. `Update`, `Hardware` and `xART` are recognised as *not*
/// interesting on purpose — they are real macOS roles Broza must never write
/// to, so they collapse into `unknown` like anything else.
///
/// An empty array is `unknown` here; [`volume_role`] is the function that knows
/// when a roleless volume is a plain user volume.
pub fn roles_to_volume_role(roles: &[String]) -> VolumeRole {
    let mapped = roles.iter().filter_map(|role| known_role(role));
    let mut writable = None;
    for role in mapped {
        if !role.writable_by_broza() {
            return role;
        }
        writable = writable.or(Some(role));
    }
    writable.unwrap_or(VolumeRole::Unknown)
}

/// Role of a volume, given its roles, its own mount point and what macOS says.
///
/// `mount_point` must be the volume's *own* mount point. A snapshot path never
/// qualifies a volume as anything: the snapshot of a sealed system volume is
/// mounted at `/`, and a Time Machine snapshot is browsed under `/Volumes`,
/// which would otherwise make the system volume look like a user disk.
pub fn volume_role(roles: &[String], mount_point: Option<&Path>, facts: VolumeFacts<'_>) -> VolumeRole {
    // Time Machine's own answer outranks everything, including a declared
    // role: a volume it backs up to is a backup volume, and Broza writes to
    // backups only through `tmutil` (`AGENTS.md` §2.3).
    if facts.backup_evidence == BackupEvidence::Volume {
        return VolumeRole::Backup;
    }
    let declared = roles_to_volume_role(roles);
    if declared != VolumeRole::Unknown || !roles.is_empty() {
        return declared;
    }
    if is_time_machine_destination(facts) {
        return VolumeRole::Backup;
    }
    if facts.writable_volume && mount_point.is_some_and(is_user_volume_mount_point) {
        return VolumeRole::User;
    }
    VolumeRole::Unknown
}

/// `true` when everything Broza can see about a *roleless* volume says "Time
/// Machine destination": a destination of that name, a backup directory at
/// the root, or a name or content token that says so.
///
/// A backup volume is writable and sits under `/Volumes` like any other, so
/// without this check it would be classified `user` and become writable. It is
/// not: backups are only ever changed through `tmutil` (`AGENTS.md` §2.3).
fn is_time_machine_destination(facts: VolumeFacts<'_>) -> bool {
    facts.backup_evidence == BackupEvidence::Name
        || facts.time_machine_marker
        || [facts.content, facts.name].into_iter().flatten().any(mentions_time_machine)
}

/// `true` when a content token or a volume name names Time Machine.
fn mentions_time_machine(text: &str) -> bool {
    TIME_MACHINE_HINTS.iter().any(|hint| text.contains(hint))
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

/// `true` for a path macOS mounts a user volume at: a direct child of `/Volumes`.
///
/// Exactly one level down. `/Volumes/Backup/inner` is a directory inside a
/// volume, not a volume, and a mount point Broza cannot place is not a user
/// volume.
fn is_user_volume_mount_point(mount_point: &Path) -> bool {
    mount_point.parent() == Some(Path::new(USER_VOLUMES_ROOT))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{BackupEvidence, VolumeFacts, roles_to_volume_role, volume_role};
    use crate::model::VolumeRole;

    fn roles(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|token| (*token).to_owned()).collect()
    }

    /// What a plain external disk looks like: writable, nothing else known.
    fn writable() -> VolumeFacts<'static> {
        VolumeFacts { writable_volume: true, ..VolumeFacts::default() }
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
    fn a_protected_role_wins_over_a_writable_one_whatever_the_order() {
        let cases = [
            (vec!["Data", "System"], VolumeRole::System),
            (vec!["System", "Data"], VolumeRole::System),
            (vec!["Data", "Backup"], VolumeRole::Backup),
            (vec!["Data", "Preboot"], VolumeRole::Preboot),
            (vec!["Update", "System"], VolumeRole::System),
            (vec!["Update", "Data"], VolumeRole::Data),
        ];
        for (tokens, expected) in cases {
            let role = roles_to_volume_role(&roles(&tokens));
            assert_eq!(role, expected, "{tokens:?}");
        }
    }

    #[test]
    fn a_volume_with_no_role_that_macos_calls_writable_under_volumes_is_a_user_volume() {
        let mount = Path::new("/Volumes/Backup Disk");

        let role = volume_role(&[], Some(mount), writable());

        assert_eq!(role, VolumeRole::User);
        assert!(role.writable_by_broza());
    }

    #[test]
    fn a_read_only_volume_under_volumes_is_never_a_user_volume() {
        let mount = Path::new("/Volumes/Installer");

        let role = volume_role(&[], Some(mount), VolumeFacts::default());

        assert_eq!(role, VolumeRole::Unknown, "a read-only installer image is not the user's disk");
        assert!(!role.writable_by_broza());
    }

    #[test]
    fn a_writable_volume_anywhere_but_directly_under_volumes_stays_unknown() {
        let cases = [
            None,
            Some(Path::new("/")),
            Some(Path::new("/Volumes")),
            Some(Path::new("/Volumes/Disk/nested")),
            Some(Path::new("/System/Volumes/Data")),
            Some(Path::new("/private/var/folders/aa/bb/T/mounted")),
        ];
        for mount_point in cases {
            assert_eq!(volume_role(&[], mount_point, writable()), VolumeRole::Unknown, "{mount_point:?}");
        }
    }

    #[test]
    fn a_time_machine_destination_is_a_backup_volume_and_not_a_user_volume() {
        let mount = Path::new("/Volumes/Time Capsule");
        let by_marker = VolumeFacts { time_machine_marker: true, ..writable() };
        let by_name = VolumeFacts { name: Some("Time Machine Backups"), ..writable() };
        let by_content = VolumeFacts { content: Some("com.apple.TimeMachine.backup"), ..writable() };

        for facts in [by_marker, by_name, by_content] {
            let role = volume_role(&[], Some(mount), facts);
            assert_eq!(role, VolumeRole::Backup, "{facts:?}");
            assert!(!role.writable_by_broza(), "{facts:?}");
        }
    }

    #[test]
    fn a_volume_time_machine_backs_up_to_is_a_backup_volume_whatever_it_is_called() {
        let mount = Path::new("/Volumes/Backup4TB");
        let facts = VolumeFacts { backup_evidence: BackupEvidence::Volume, ..writable() };

        let role = volume_role(&[], Some(mount), facts);

        assert_eq!(role, VolumeRole::Backup, "a writable APFS destination with a chosen name");
        assert!(!role.writable_by_broza());
    }

    #[test]
    fn time_machine_outranks_even_a_declared_role_when_it_names_the_volume_itself() {
        let facts = VolumeFacts { backup_evidence: BackupEvidence::Volume, ..writable() };

        assert_eq!(volume_role(&roles(&["Data"]), Some(Path::new("/x")), facts), VolumeRole::Backup);
    }

    #[test]
    fn a_destination_that_only_shares_a_name_never_demotes_a_volume_with_a_role() {
        let by_name = VolumeFacts { backup_evidence: BackupEvidence::Name, ..writable() };

        assert_eq!(
            volume_role(&roles(&["Data"]), Some(Path::new("/System/Volumes/Data")), by_name),
            VolumeRole::Data,
            "a network destination called `Data` must not demote the boot data volume"
        );
        assert_eq!(volume_role(&roles(&["System"]), Some(Path::new("/")), by_name), VolumeRole::System);
    }

    #[test]
    fn a_destination_that_shares_a_name_does_claim_a_volume_with_no_role() {
        let by_name = VolumeFacts { backup_evidence: BackupEvidence::Name, ..writable() };

        let role = volume_role(&[], Some(Path::new("/Volumes/Backup4TB")), by_name);

        assert_eq!(role, VolumeRole::Backup, "the alternative reading would be `user`");
        assert!(!role.writable_by_broza());
    }

    #[test]
    fn a_named_role_is_never_overridden_by_the_mount_point_or_the_facts() {
        let mount = Path::new("/Volumes/Something");

        assert_eq!(volume_role(&roles(&["System"]), Some(mount), writable()), VolumeRole::System);
        assert_eq!(
            volume_role(&roles(&["Update"]), Some(mount), writable()),
            VolumeRole::Unknown,
            "a role Broza does not model must not become a writable user volume"
        );
    }
}
