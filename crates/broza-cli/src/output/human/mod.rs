//! Human-readable renderings, one module per command.
//!
//! These modules own the shape of what a person reads; the `--json` and
//! `--csv` forms of the same data go through [`crate::output::render`]. Every
//! function here is pure: a report and a [`ColorPolicy`](crate::output::ColorPolicy)
//! in, a `String` out, so a layout can be snapshot-tested without a terminal.

pub mod explain;
pub mod scan;

use broza::model::{Action, Risk};

use crate::output::Style;

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
    fn every_action_is_described_in_words() {
        for action in [Action::Quarantine, Action::Purge, Action::TmutilDelete, Action::InformOnly] {
            let label = action_label(action);
            assert!(label.len() > 10, "{action:?}: {label}");
            assert!(!label.contains('_'), "{action:?}: {label} must not be a contract token");
        }
    }
}
