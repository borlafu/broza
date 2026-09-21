//! What `broza explain` says, and the JSON shape it says it in
//! (`docs/cli-spec.md` §3.2 and §4.7).
//!
//! Three kinds of target share one payload. A **volume** is explained by its
//! APFS role, whose paragraphs live with the rest of the role wording in
//! [`crate::adapters::diskutil::purpose`]. A **category** is explained by the
//! prose in [`super::category_text`], and carries the risk and action of the
//! table in `docs/cli-spec.md` §3.3. A **path** is explained by the volume it
//! lives on, with the path itself recorded so a caller can see which volume
//! answered.
//!
//! Everything here is a pure function of its input: no filesystem, no
//! processes. Resolving a target to a volume is the caller's job.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::adapters::diskutil::purpose::RoleExplanation;
use crate::model::{Action, Category, Risk, Volume};

use super::category_text::{CategoryText, text_for};

/// The three paragraphs `broza explain` prints, in the order it prints them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Explanation {
    /// Answer to "What it is:".
    pub what_it_is: String,
    /// Answer to "What it is for:".
    pub what_it_is_for: String,
    /// Answer to "Is it safe to touch?".
    pub is_it_safe: String,
}

impl From<RoleExplanation> for Explanation {
    fn from(role: RoleExplanation) -> Self {
        Self {
            what_it_is: role.what_it_is.to_owned(),
            what_it_is_for: role.what_it_is_for.to_owned(),
            is_it_safe: role.is_it_safe.to_owned(),
        }
    }
}

impl From<CategoryText> for Explanation {
    fn from(text: CategoryText) -> Self {
        Self {
            what_it_is: text.what_it_is.to_owned(),
            what_it_is_for: text.what_it_is_for.to_owned(),
            is_it_safe: text.is_it_safe.to_owned(),
        }
    }
}

/// Which kind of target an [`ExplainReport`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExplainKind {
    /// A volume, identified by device id, name or mount point.
    Volume,
    /// A cleanup category of `docs/cli-spec.md` §3.3.
    Category,
    /// A filesystem path, explained through the volume it lives on.
    Path,
}

/// `data` payload of `broza explain --json` (`docs/cli-spec.md` §4.7).
///
/// Optional fields are absent rather than null, as §4.1 requires: a category
/// explanation carries no volume, and a volume explanation no risk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ExplainReport {
    /// What was explained.
    pub kind: ExplainKind,
    /// The volume, for `volume` and `path` targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<Volume>,
    /// The category, for `category` targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    /// The path the user asked about, for `path` targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// The three paragraphs.
    pub explanation: Explanation,
    /// Base risk of the category; absent for volumes and paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<Risk>,
    /// Default action of the category; absent for volumes and paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
}

impl ExplainReport {
    /// Explain `volume` through the paragraphs of its role.
    pub fn for_volume(volume: Volume) -> Self {
        let explanation = explain_volume(&volume);
        Self {
            kind: ExplainKind::Volume,
            volume: Some(volume),
            category: None,
            path: None,
            explanation,
            risk: None,
            action: None,
        }
    }

    /// Explain `category`, with the risk and action of `docs/cli-spec.md` §3.3.
    pub fn for_category(category: Category) -> Self {
        Self {
            kind: ExplainKind::Category,
            volume: None,
            category: Some(category),
            path: None,
            explanation: explain_category(category),
            risk: Some(category.base_risk()),
            action: Some(category.default_action()),
        }
    }

    /// Explain `path` through the `volume` it was resolved to.
    pub fn for_path(path: &Path, volume: Volume) -> Self {
        Self { kind: ExplainKind::Path, path: Some(path.to_path_buf()), ..Self::for_volume(volume) }
    }
}

/// The paragraphs for the role of `volume`.
pub fn explain_volume(volume: &Volume) -> Explanation {
    crate::adapters::diskutil::purpose::explain_role(volume.role).into()
}

/// The paragraphs for `category`.
pub fn explain_category(category: Category) -> Explanation {
    text_for(category).into()
}

/// The one-line summary `--short` prints for `category`.
pub fn category_summary(category: Category) -> &'static str {
    text_for(category).summary
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        ExplainKind, ExplainReport, Explanation, category_summary, explain_category, explain_volume,
    };
    use crate::model::{Action, Category, Risk, Volume, VolumeRole};

    fn volume(role: VolumeRole) -> Volume {
        Volume {
            id: "disk3s5".parse().unwrap_or_else(|e| panic!("{e}")),
            name: "Macintosh HD - Data".to_owned(),
            role,
            mount_point: Some(PathBuf::from("/System/Volumes/Data")),
            used_bytes: 798_210_000_000,
            writable_by_broza: role.writable_by_broza(),
            purpose: "Your data.".to_owned(),
        }
    }

    fn is_a_sentence(text: &str) -> bool {
        text.len() > 40 && text.ends_with('.') && text.starts_with(|c: char| c.is_uppercase())
    }

    #[test]
    fn every_category_has_three_paragraphs_and_a_summary() {
        for category in Category::all() {
            let explanation = explain_category(category);
            assert!(is_a_sentence(&explanation.what_it_is), "{category}: {}", explanation.what_it_is);
            assert!(is_a_sentence(&explanation.what_it_is_for), "{category}");
            assert!(is_a_sentence(&explanation.is_it_safe), "{category}");
            let summary = category_summary(category);
            assert!(summary.ends_with('.'), "{category}: {summary}");
            assert!(!summary.contains('\n'), "{category}: a summary is one line");
        }
    }

    #[test]
    fn every_category_explanation_is_distinct() {
        let mut summaries: Vec<&str> = Category::all().iter().map(|c| category_summary(*c)).collect();
        summaries.sort_unstable();
        let total = summaries.len();
        summaries.dedup();
        assert_eq!(summaries.len(), total, "two categories share a summary");
    }

    /// The `--short` example of `docs/cli-spec.md` §3.2, word for word.
    #[test]
    fn the_snapshots_summary_is_the_one_in_the_specification() {
        assert_eq!(
            category_summary(Category::Snapshots),
            "APFS local snapshots (Time Machine, OS update). Managed only via tmutil; sizes not \
             reported by macOS."
        );
    }

    #[test]
    fn a_category_report_carries_its_risk_and_action() {
        let report = ExplainReport::for_category(Category::CloudSynced);

        assert_eq!(report.kind, ExplainKind::Category);
        assert_eq!(report.risk, Some(Risk::Red));
        assert_eq!(report.action, Some(Action::InformOnly));
        assert_eq!(report.volume, None);
        assert_eq!(report.path, None);
    }

    #[test]
    fn a_volume_report_carries_no_risk_and_the_role_paragraphs() {
        let report = ExplainReport::for_volume(volume(VolumeRole::Data));

        assert_eq!(report.kind, ExplainKind::Volume);
        assert_eq!(report.risk, None);
        assert_eq!(report.action, None);
        assert_eq!(report.category, None);
        assert_eq!(report.explanation, explain_volume(&volume(VolumeRole::Data)));
    }

    #[test]
    fn a_path_report_keeps_both_the_path_and_the_volume_it_lives_on() {
        let report = ExplainReport::for_path(Path::new("/Users/dana"), volume(VolumeRole::Data));

        assert_eq!(report.kind, ExplainKind::Path);
        assert_eq!(report.path, Some(PathBuf::from("/Users/dana")));
        assert_eq!(report.volume.map(|v| v.id.to_string()), Some("disk3s5".to_owned()));
    }

    #[test]
    fn absent_fields_are_omitted_rather_than_null() {
        let json = serde_json::to_value(ExplainReport::for_category(Category::Trash))
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(json["kind"], "category");
        assert_eq!(json["category"], "trash");
        assert!(json.get("volume").is_none(), "{json}");
        assert!(json.get("path").is_none(), "{json}");
        assert!(json["explanation"]["what_it_is"].is_string());
    }

    #[test]
    fn a_report_round_trips_through_json() {
        let report = ExplainReport::for_path(Path::new("/Users"), volume(VolumeRole::Data));
        let json = serde_json::to_value(&report).unwrap_or_else(|e| panic!("{e}"));
        let back: ExplainReport = serde_json::from_value(json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back, report);
    }

    #[test]
    fn a_role_explanation_becomes_the_serde_type_unchanged() {
        let role = crate::adapters::diskutil::purpose::explain_role(VolumeRole::System);
        let explanation: Explanation = role.into();
        assert_eq!(explanation.what_it_is, role.what_it_is);
        assert_eq!(explanation.is_it_safe, role.is_it_safe);
    }
}
