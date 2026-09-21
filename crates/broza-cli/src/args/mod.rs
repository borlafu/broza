//! Per-command argument structs (`docs/cli-spec.md` §3).
//!
//! Sizes and durations are parsed as `String` here and validated by the core
//! when the command runs, so that `--help` stays free of parser jargon and the
//! error messages come from a single place.

pub mod clean;
pub mod config;
pub mod explain;
pub mod quarantine;
pub mod restore;
pub mod scan;
pub mod suggest;

use clap::ValueEnum;

pub use clean::CleanArgs;
pub use config::{ConfigArgs, ConfigCommand};
pub use explain::ExplainArgs;
pub use quarantine::{QuarantineArgs, QuarantineCommand};
pub use restore::RestoreArgs;
pub use scan::ScanArgs;
pub use suggest::SuggestArgs;

/// Risk level of a finding (`docs/cli-spec.md` §4.1, enum `risk`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum RiskLevel {
    /// Safe to reclaim.
    Green,
    /// Review before reclaiming.
    Amber,
    /// Information only; Broza never deletes these.
    Red,
}

/// `--risk` on `suggest`, which additionally accepts `all`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum RiskFilter {
    /// Only green findings.
    Green,
    /// Only amber findings.
    Amber,
    /// Only red findings.
    Red,
    /// No filtering.
    #[default]
    All,
}

/// Split a repeatable, comma-separated `--category` list into single ids.
pub fn split_categories(raw: &[String]) -> Vec<String> {
    raw.iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn categories_accept_both_repetition_and_commas() {
        let raw = vec!["build-cache,user-cache".to_owned(), " duplicates ".to_owned()];
        assert_eq!(
            split_categories(&raw),
            vec!["build-cache".to_owned(), "user-cache".to_owned(), "duplicates".to_owned(),]
        );
    }

    #[test]
    fn empty_segments_are_dropped() {
        assert_eq!(split_categories(&["a,,b,".to_owned()]), vec!["a".to_owned(), "b".to_owned()]);
        assert!(split_categories(&[]).is_empty());
    }
}
