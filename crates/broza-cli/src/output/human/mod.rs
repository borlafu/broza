//! Human-readable renderings, one module per command.
//!
//! These modules own the shape of what a person reads; the `--json` and
//! `--csv` forms of the same data go through [`crate::output::render`]. Every
//! function here is pure: a report and a [`ColorPolicy`](crate::output::ColorPolicy)
//! in, a `String` out, so a layout can be snapshot-tested without a terminal.

pub mod clean;
pub mod explain;
pub mod scan;
pub mod suggest;

use broza::model::{Action, Risk, VolumeRole};

use crate::output::{ColorPolicy, Style, paint};

/// Text label of a risk level.
///
/// `docs/cli-spec.md` §3.3 makes this mandatory: risk is *always* rendered as a
/// word, and colour is only ever added on top of it (RNF-06).
pub const fn risk_label(risk: Risk) -> &'static str {
    match risk {
        Risk::Green => "SAFE",
        Risk::Amber => "REVIEW",
        _ => "INFO ONLY",
    }
}

/// Colour of a risk level, used only where [`risk_label`] is printed too.
pub const fn risk_style(risk: Risk) -> Style {
    match risk {
        Risk::Green => Style::Green,
        Risk::Amber => Style::Yellow,
        _ => Style::Red,
    }
}

/// The risk of a finding as one coloured word: [`risk_label`] plus its colour.
///
/// The single place a risk is turned into text, so `scan`, `explain` and the
/// confirmation prompt can never drift into calling the same level by two
/// different names.
pub fn risk_chip(policy: ColorPolicy, risk: Risk) -> String {
    paint(policy, risk_style(risk), risk_label(risk))
}

/// How a volume's name and role are emphasised, everywhere they are printed.
///
/// What is yours stands out and what Broza may never write to recedes: `data`
/// and `user` volumes are bold, every protected role is dim. Colour only
/// repeats what the role label already says in words (RNF-06).
pub const fn role_style(role: VolumeRole) -> Style {
    match role {
        VolumeRole::Data | VolumeRole::User => Style::Bold,
        _ => Style::Dim,
    }
}

/// What Broza would do with a finding, in words rather than in contract tokens.
pub const fn action_label(action: Action) -> &'static str {
    match action {
        Action::Quarantine => "move to quarantine, reversible until it expires",
        Action::Purge => "delete irreversibly, with a typed confirmation",
        Action::TmutilDelete => "delete through tmutil",
        _ => "report only, Broza never deletes this",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn every_risk_level_has_a_word_and_a_colour_of_its_own() {
        assert_eq!(risk_label(Risk::Green), "SAFE");
        assert_eq!(risk_label(Risk::Amber), "REVIEW");
        assert_eq!(risk_label(Risk::Red), "INFO ONLY");
        assert_eq!(risk_style(Risk::Green), Style::Green);
        assert_eq!(risk_style(Risk::Amber), Style::Yellow);
        assert_eq!(risk_style(Risk::Red), Style::Red);
    }

    #[test]
    fn a_risk_chip_is_the_label_plus_colour_and_never_less() {
        for risk in [Risk::Green, Risk::Amber, Risk::Red] {
            assert_eq!(risk_chip(ColorPolicy::Never, risk), risk_label(risk));
            let coloured = risk_chip(ColorPolicy::Always, risk);
            assert!(coloured.contains(risk_label(risk)), "{coloured:?}");
            assert!(coloured.contains('\u{1b}'), "{coloured:?}");
        }
    }

    #[test]
    fn the_users_own_volumes_are_bold_and_every_protected_role_is_dim() {
        for role in [VolumeRole::Data, VolumeRole::User] {
            assert_eq!(role_style(role), Style::Bold, "{role:?}");
        }
        for role in [
            VolumeRole::System,
            VolumeRole::Preboot,
            VolumeRole::Recovery,
            VolumeRole::Vm,
            VolumeRole::Backup,
            VolumeRole::Unknown,
        ] {
            assert_eq!(role_style(role), Style::Dim, "{role:?}");
        }
    }

    #[test]
    fn every_action_is_described_in_words() {
        for action in [Action::Quarantine, Action::Purge, Action::TmutilDelete, Action::InformOnly] {
            let label = action_label(action);
            assert!(label.len() > 10, "{action:?}: {label}");
            assert!(!label.contains('_'), "{action:?}: {label} must not be a contract token");
        }
    }
}
