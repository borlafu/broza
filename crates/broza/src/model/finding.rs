//! Findings produced by the detectors (`docs/cli-spec.md` §4.3).

use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::BrozaError;
use crate::model::category::Category;
use crate::model::disk::Snapshot;
use crate::model::finding_builder::FindingBuilder;
use crate::model::ids::FindingId;

/// Risk level of a finding. Ordered: `green < amber < red`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Risk {
    /// Safe to remove: regenerable data.
    Green,
    /// Needs review: removable, but the user may still want it.
    Amber,
    /// Information only, or destructive; Broza never removes `red` items silently.
    Red,
}

/// What Broza would do with a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Action {
    /// Move to the quarantine store; reversible until expiry or purge.
    Quarantine,
    /// Delete irreversibly. Requires `--purge` and the literal confirmation.
    Purge,
    /// Delete an APFS snapshot through `tmutil`.
    TmutilDelete,
    /// Report only. Broza never touches these items.
    InformOnly,
}

impl Action {
    /// `true` when the action removes data. Mirrors [`Finding::actionable`].
    pub const fn is_actionable(self) -> bool {
        !matches!(self, Self::InformOnly)
    }
}

/// One path contributing to a finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FindingPath {
    /// Absolute path.
    pub path: PathBuf,
    /// Size of the path in bytes.
    pub size_bytes: u64,
    /// `max(atime, kMDItemLastUsedDate)`. Absent when neither is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<Timestamp>,
}

/// The provider's official steps for an `inform_only` finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Instructions {
    /// Name of the provider, for example `iCloud Drive`.
    pub provider: String,
    /// One-sentence summary of what the user should do.
    pub summary: String,
    /// Ordered steps, as written by the provider.
    #[serde(default)]
    pub steps: Vec<String>,
}

/// Something Broza found and can explain.
///
/// Findings are built through [`Finding::builder`], which enforces the invariants
/// checked by [`Finding::validate`]. The fields are public because this type is the
/// JSON contract; anything that edits them must re-validate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Finding {
    /// Stable identifier `category.detector`.
    pub id: FindingId,
    /// Category the finding belongs to.
    pub category: Category,
    /// Short human title.
    pub title: String,
    /// Longer explanation of what the data is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Risk level.
    pub risk: Risk,
    /// Bytes that removing the finding would reclaim. `0` when macOS does not report a size.
    pub reclaimable_bytes: u64,
    /// Number of items behind the finding, when the detector counts them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_count: Option<u64>,
    /// `false` when Broza cannot remove the finding; the GUI must disable the control.
    pub actionable: bool,
    /// Action Broza proposes.
    pub action: Action,
    /// Why the detector reached this conclusion (`--explain`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Paths behind the finding.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<FindingPath>,
    /// Snapshots behind the finding. Only for [`Category::Snapshots`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snapshots: Vec<Snapshot>,
    /// Provider steps. Only for [`Action::InformOnly`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<Instructions>,
}

impl Finding {
    /// Start building a finding with the defaults of its category.
    pub fn builder(id: FindingId, category: Category, title: impl Into<String>) -> FindingBuilder {
        FindingBuilder::new(id, category, title)
    }

    /// Check the invariants of `docs/cli-spec.md` §3.3 and §4.3.
    ///
    /// 1. The identifier's category part matches `category`.
    /// 2. [`Category::CloudSynced`] implies `inform_only`, not actionable and `red`.
    /// 3. `actionable` is exactly `action != inform_only`.
    /// 4. `instructions` are present only for `inform_only` findings.
    /// 5. `snapshots` are present only for [`Category::Snapshots`].
    pub fn validate(&self) -> Result<(), BrozaError> {
        let inconsistent = |reason: &str| Err(BrozaError::Other(format!("finding `{}`: {reason}", self.id)));
        if self.id.category_part() != self.category.as_str() {
            return inconsistent("identifier does not start with its category");
        }
        if self.category.is_inform_only()
            && (self.action != Action::InformOnly || self.actionable || self.risk != Risk::Red)
        {
            return inconsistent("cloud-synced findings are always red, inform-only and not actionable");
        }
        if self.actionable != self.action.is_actionable() {
            return inconsistent("`actionable` must be `action != inform_only`");
        }
        if self.instructions.is_some() && self.action != Action::InformOnly {
            return inconsistent("`instructions` are only allowed on inform-only findings");
        }
        if !self.snapshots.is_empty() && self.category != Category::Snapshots {
            return inconsistent("`snapshots` are only allowed on the snapshots category");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, Finding, FindingPath, Instructions, Risk};
    use crate::model::category::Category;
    use crate::model::disk::Snapshot;

    fn finding(id: &str, category: Category) -> Finding {
        Finding::builder(id.parse().unwrap_or_else(|e| panic!("{e}")), category, "title")
            .build()
            .unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn risk_is_ordered_from_green_to_red() {
        assert!(Risk::Green < Risk::Amber);
        assert!(Risk::Amber < Risk::Red);
        let mut risks = [Risk::Red, Risk::Green, Risk::Amber];
        risks.sort_unstable();
        assert_eq!(risks, [Risk::Green, Risk::Amber, Risk::Red]);
    }

    #[test]
    fn actions_use_the_stable_identifiers_of_the_specification() {
        let cases = [
            (Action::Quarantine, "\"quarantine\"", true),
            (Action::Purge, "\"purge\"", true),
            (Action::TmutilDelete, "\"tmutil_delete\"", true),
            (Action::InformOnly, "\"inform_only\"", false),
        ];
        for (action, expected, actionable) in cases {
            assert_eq!(serde_json::to_string(&action).unwrap_or_else(|e| panic!("{e}")), expected);
            assert_eq!(action.is_actionable(), actionable);
        }
        assert!(serde_json::from_str::<Action>("\"shred\"").is_err());
    }

    #[test]
    fn a_finding_built_from_its_category_is_valid() {
        for category in Category::all() {
            let id = format!("{category}.detector");
            let built = finding(&id, category);
            assert_eq!(built.risk, category.base_risk());
            assert_eq!(built.action, category.default_action());
            assert_eq!(built.actionable, category.default_action().is_actionable());
            assert!(built.validate().is_ok(), "{id}");
        }
    }

    #[test]
    fn validate_rejects_inconsistent_findings() {
        let base = finding("user-cache.logs", Category::UserCache);

        let wrong_id = Finding { category: Category::Trash, ..base.clone() };
        assert!(wrong_id.validate().is_err());

        let wrong_flag = Finding { actionable: false, ..base.clone() };
        assert!(wrong_flag.validate().is_err());

        let stray_instructions = Finding {
            instructions: Some(Instructions {
                provider: "iCloud Drive".into(),
                summary: "s".into(),
                steps: vec!["one".into()],
            }),
            ..base.clone()
        };
        assert!(stray_instructions.validate().is_err());

        let stray_snapshots = Finding {
            snapshots: vec![Snapshot { name: "s".into(), uuid: None, purgeable: true }],
            ..base.clone()
        };
        assert!(stray_snapshots.validate().is_err());

        let cloud = finding("cloud-synced.icloud", Category::CloudSynced);
        let downgraded = Finding { action: Action::Purge, actionable: true, ..cloud };
        assert!(downgraded.validate().is_err());
    }

    #[test]
    fn a_path_without_a_last_used_timestamp_omits_the_field() {
        let path = FindingPath { path: "/tmp/x".into(), size_bytes: 10, last_used: None };
        let json = serde_json::to_value(&path).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(json, serde_json::json!({"path": "/tmp/x", "size_bytes": 10}));
    }

    #[test]
    fn timestamps_serialize_as_rfc_3339_in_utc() {
        let raw =
            serde_json::json!({"path": "/tmp/x", "size_bytes": 10, "last_used": "2026-06-15T09:12:00Z"});
        let path: FindingPath = serde_json::from_value(raw.clone()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(serde_json::to_value(&path).unwrap_or_else(|e| panic!("{e}")), raw);
        assert!(
            serde_json::from_str::<FindingPath>("{\"path\":\"/x\",\"size_bytes\":0,\"last_used\":\"nope\"}")
                .is_err()
        );
    }
}
