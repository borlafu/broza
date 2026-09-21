//! Plain-language text for volume roles (RF-04).
//!
//! Two lengths of the same idea. [`purpose_for`] is the one sentence that goes
//! into the `purpose` field of every volume in the JSON contract
//! (`docs/cli-spec.md` §4.2); [`explain_role`] is the three paragraphs
//! `broza explain <volume>` prints (§3.2). Both are pure functions of the role,
//! so the wording can be reviewed as text and tested as data.
//!
//! All text is English (`docs/adr/0005-english-everywhere.md`).

use crate::model::VolumeRole;

/// Name used for a volume macOS reports without one.
const UNTITLED_VOLUME: &str = "This volume";

/// The three paragraphs `broza explain` prints for a volume role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleExplanation {
    /// What the volume is, in the words a non-specialist uses.
    pub what_it_is: &'static str,
    /// What it is for, and how much of it is the user's own data.
    pub what_it_is_for: &'static str,
    /// Whether Broza, or the user, may touch it.
    pub is_it_safe: &'static str,
}

/// One sentence describing what a volume is for.
///
/// `name` is only used for volumes whose role says nothing about them — the
/// ones the user mounted — where the name is the only thing that identifies the
/// volume to its owner.
pub fn purpose_for(role: VolumeRole, name: &str) -> String {
    match role {
        VolumeRole::System => "Read-only, sealed and signed operating system volume (SSV).".to_owned(),
        VolumeRole::Data => {
            "The writable volume that holds your home folder, your applications and your settings.".to_owned()
        }
        VolumeRole::Preboot => {
            "Boot loader data that macOS needs before the system volume is available.".to_owned()
        }
        VolumeRole::Recovery => "The recovery environment used to repair or reinstall macOS.".to_owned(),
        VolumeRole::Vm => {
            "Virtual memory: the swap and sleep-image files macOS writes and manages on its own.".to_owned()
        }
        VolumeRole::Backup => {
            "A Time Machine backup volume, which is only ever changed through `tmutil`.".to_owned()
        }
        VolumeRole::User => {
            format!("{} is a volume of your own, outside the macOS system group.", subject(name))
        }
        _ => "A volume with a role Broza does not recognise, so it is treated as read-only.".to_owned(),
    }
}

/// One sentence for a volume, preferring what its role token or name says.
///
/// Several volumes of a normal macOS install carry a role Broza does not model
/// — `Update`, `Hardware`, `xART` — and one, `iSCPreboot`, is only
/// recognisable by its name. Their *role* stays `unknown`, because that is what
/// decides write protection, but "a volume with a role Broza does not
/// recognise" is a poor thing to print four times in one `scan`. When a token
/// or the name is one Broza can name, it supplies the sentence instead.
///
/// The name is only consulted for a volume whose role is `unknown`. A role
/// Broza does model already describes the volume better than its name could,
/// and a user is free to call an external disk `Hardware`.
pub fn purpose_for_volume(role: VolumeRole, roles: &[String], name: &str) -> String {
    let by_name = (role == VolumeRole::Unknown).then_some(name);
    roles
        .iter()
        .map(String::as_str)
        .chain(by_name)
        .find_map(purpose_for_token)
        .map_or_else(|| purpose_for(role, name), ToOwned::to_owned)
}

/// The sentence for a role token or volume name Broza can name but not model.
pub fn purpose_for_token(token: &str) -> Option<&'static str> {
    match token {
        "Update" => Some("Staging area for macOS updates, written and cleared by the installer."),
        "Hardware" => Some("Hardware-specific data written by the firmware of this Mac."),
        "xART" => Some("Secure counters used by the Secure Enclave to detect rollback attacks."),
        "iSCPreboot" => {
            Some("Boot loader data for the internal storage controller, managed entirely by macOS.")
        }
        _ => None,
    }
}

/// The three paragraphs of `broza explain` for a role.
pub fn explain_role(role: VolumeRole) -> RoleExplanation {
    match role {
        VolumeRole::System => SYSTEM,
        VolumeRole::Data => DATA,
        VolumeRole::Preboot => PREBOOT,
        VolumeRole::Recovery => RECOVERY,
        VolumeRole::Vm => VM,
        VolumeRole::Backup => BACKUP,
        VolumeRole::User => USER,
        _ => UNKNOWN,
    }
}

/// How a sentence refers to a volume: by its name, or impersonally.
fn subject(name: &str) -> &str {
    let trimmed = name.trim();
    if trimmed.is_empty() { UNTITLED_VOLUME } else { trimmed }
}

/// Explanation of the sealed system volume.
const SYSTEM: RoleExplanation = RoleExplanation {
    what_it_is: "The volume macOS itself lives on. It is sealed and cryptographically signed, and \
the system boots from a snapshot of it rather than from the volume directly.",
    what_it_is_for: "It holds the operating system and nothing of yours. Its size barely changes \
between updates, and nothing you delete elsewhere makes it smaller.",
    is_it_safe: "No. It is read-only even for the administrator, and Broza never writes to it. \
Freeing space here is not possible and not necessary.",
};

/// Explanation of the data volume, following `docs/cli-spec.md` §3.2.
const DATA: RoleExplanation = RoleExplanation {
    what_it_is: "The mutable data volume of macOS. It holds /Users, the applications you install, \
your user libraries and caches. It is joined to the system volume through \"firmlinks\", which is \
why Finder shows a single disk.",
    what_it_is_for: "This is where practically everything that belongs to you lives. Around 99% of \
what Broza can help you clean is here.",
    is_it_safe: "Yes, with judgement. It is not a system volume. Broza never modifies the System, \
Preboot, Recovery or VM volumes.",
};

/// Explanation of the preboot volume.
const PREBOOT: RoleExplanation = RoleExplanation {
    what_it_is: "A small volume holding what the Mac needs before macOS itself can run: boot \
loaders, FileVault unlock data and per-system startup files.",
    what_it_is_for: "It exists so an encrypted Mac can show a login window before the data volume \
is unlocked. One copy is kept per installed system.",
    is_it_safe: "No. Broza never writes to it, and deleting anything here can leave the Mac unable \
to start. It is usually a few gigabytes and it is supposed to be.",
};

/// Explanation of the recovery volume.
const RECOVERY: RoleExplanation = RoleExplanation {
    what_it_is: "The recovery environment: a minimal macOS you reach by starting the Mac with the \
power button held down.",
    what_it_is_for: "It is what reinstalls macOS, repairs disks and restores from Time Machine \
when the main system will not start.",
    is_it_safe: "No. Broza never writes to it. It is the safety net you want intact on the day \
something else goes wrong.",
};

/// Explanation of the virtual memory volume.
const VM: RoleExplanation = RoleExplanation {
    what_it_is: "The volume macOS uses for virtual memory: swap files and, on a Mac that sleeps to \
disk, the sleep image.",
    what_it_is_for: "It grows and shrinks on its own as memory pressure changes. Its size reflects \
how much memory your applications asked for, not files you forgot about.",
    is_it_safe: "No. Broza never writes to it. Deleting swap by hand risks a crash, and macOS \
reclaims the space itself once the pressure is gone.",
};

/// Explanation of a Time Machine backup volume.
const BACKUP: RoleExplanation = RoleExplanation {
    what_it_is: "A Time Machine destination: a volume holding backups of this Mac, or of another \
one, as APFS snapshots.",
    what_it_is_for: "It is the copy of your data that survives losing the Mac. Old backups are \
thinned automatically as the volume fills.",
    is_it_safe: "Only through Time Machine. Broza never writes to a backup volume directly; \
removing old backups is done with `tmutil`, which keeps the backup database consistent.",
};

/// Explanation of a volume the user mounted.
const USER: RoleExplanation = RoleExplanation {
    what_it_is: "A volume with no macOS role: an external disk, a disk image, or a second volume \
you created yourself.",
    what_it_is_for: "Whatever you put on it. macOS does not manage its contents and nothing here \
is needed to start the Mac.",
    is_it_safe: "Yes, with the same judgement you would use in Finder. Broza may move items from \
it to quarantine, and quarantine always stays on the same volume as the item.",
};

/// Explanation of a role Broza does not model.
const UNKNOWN: RoleExplanation = RoleExplanation {
    what_it_is: "A volume whose role this version of Broza does not recognise. macOS reported a \
role, or none at all, that is not one of the roles Broza knows.",
    what_it_is_for: "Broza cannot say. It is reported so that its size is accounted for, and \
nothing more is claimed about it.",
    is_it_safe: "Broza treats it as read-only. An unrecognised role is never writable, because the \
safe reading of \"I do not know what this is\" is \"do not touch it\".",
};

#[cfg(test)]
mod tests {
    use super::{explain_role, purpose_for, purpose_for_token, purpose_for_volume};
    use crate::model::VolumeRole;

    /// Every role the JSON contract defines (`docs/cli-spec.md` §4.1).
    const EVERY_ROLE: [VolumeRole; 8] = [
        VolumeRole::System,
        VolumeRole::Data,
        VolumeRole::Preboot,
        VolumeRole::Recovery,
        VolumeRole::Vm,
        VolumeRole::Backup,
        VolumeRole::User,
        VolumeRole::Unknown,
    ];

    #[test]
    fn every_role_has_a_purpose_that_is_one_finished_sentence() {
        for role in EVERY_ROLE {
            let purpose = purpose_for(role, "Scratch");

            assert!(purpose.ends_with('.'), "{role:?}: {purpose}");
            assert!(purpose.len() > 20, "{role:?}: {purpose}");
            assert_eq!(purpose.matches(". ").count(), 0, "{role:?} must be a single sentence");
        }
    }

    #[test]
    fn every_purpose_is_distinct_so_the_role_can_be_read_from_the_text() {
        let mut purposes: Vec<String> = EVERY_ROLE.iter().map(|role| purpose_for(*role, "x")).collect();
        purposes.sort();
        let total = purposes.len();
        purposes.dedup();

        assert_eq!(purposes.len(), total);
    }

    #[test]
    fn a_user_volume_is_named_in_its_own_purpose() {
        let purpose = purpose_for(VolumeRole::User, "Scratch");

        assert!(purpose.starts_with("Scratch is a volume of your own"), "{purpose}");
    }

    #[test]
    fn a_user_volume_without_a_name_is_still_a_sentence() {
        for name in ["", "   "] {
            let purpose = purpose_for(VolumeRole::User, name);

            assert!(purpose.starts_with("This volume is a volume of your own"), "{purpose}");
        }
    }

    #[test]
    fn a_name_the_user_padded_with_spaces_is_trimmed() {
        assert!(purpose_for(VolumeRole::User, "  Scratch  ").starts_with("Scratch "));
    }

    #[test]
    fn a_role_token_broza_can_name_but_not_model_supplies_its_own_sentence() {
        let cases = [("Update", "macOS updates"), ("Hardware", "firmware"), ("xART", "Secure Enclave")];

        for (token, expected) in cases {
            let purpose = purpose_for_volume(VolumeRole::Unknown, &[token.to_owned()], "whatever");

            assert!(purpose.contains(expected), "{token}: {purpose}");
            assert_eq!(purpose_for_token(token).map(ToOwned::to_owned), Some(purpose));
        }
    }

    #[test]
    fn a_volume_recognisable_only_by_its_name_is_named_too() {
        let purpose = purpose_for_volume(VolumeRole::Unknown, &[], "iSCPreboot");

        assert!(purpose.contains("internal storage controller"), "{purpose}");
    }

    #[test]
    fn a_name_never_overrides_the_sentence_of_a_role_broza_models() {
        let by_role = purpose_for_volume(VolumeRole::Preboot, &["Preboot".to_owned()], "iSCPreboot");
        let mistaken = purpose_for_volume(VolumeRole::User, &[], "Hardware");

        assert_eq!(by_role, purpose_for(VolumeRole::Preboot, "iSCPreboot"));
        assert_eq!(mistaken, purpose_for(VolumeRole::User, "Hardware"), "a user may name a disk anything");
    }

    #[test]
    fn a_volume_with_a_modelled_role_keeps_the_sentence_of_that_role() {
        let purpose = purpose_for_volume(VolumeRole::Data, &["Data".to_owned()], "Macintosh HD - Data");

        assert_eq!(purpose, purpose_for(VolumeRole::Data, "Macintosh HD - Data"));
        assert_eq!(purpose_for_token("Data"), None, "a modelled role is not a token exception");
    }

    #[test]
    fn every_role_is_explained_in_three_filled_paragraphs() {
        for role in EVERY_ROLE {
            let explanation = explain_role(role);

            for paragraph in [explanation.what_it_is, explanation.what_it_is_for, explanation.is_it_safe] {
                assert!(paragraph.len() > 40, "{role:?}: {paragraph}");
                assert!(paragraph.ends_with('.'), "{role:?}: {paragraph}");
            }
        }
    }

    #[test]
    fn the_data_volume_is_explained_in_the_words_of_the_specification() {
        let explanation = explain_role(VolumeRole::Data);

        assert!(explanation.what_it_is.contains("The mutable data volume of macOS."));
        assert!(explanation.what_it_is_for.contains("Around 99%"));
        assert!(explanation.is_it_safe.starts_with("Yes, with judgement."));
    }

    #[test]
    fn no_protected_role_is_explained_as_safe_to_touch() {
        let protected = [VolumeRole::System, VolumeRole::Preboot, VolumeRole::Recovery, VolumeRole::Vm];

        for role in protected {
            assert!(explain_role(role).is_it_safe.starts_with("No."), "{role:?}");
            assert!(!role.writable_by_broza(), "{role:?}");
        }
    }
}
