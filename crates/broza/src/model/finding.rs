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
    /// `true` when the action removes data. Mirrors `Finding::is_actionable`.
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
    #[serde(default, with = "crate::model::timestamp::optional", skip_serializing_if = "Option::is_none")]
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
/// The fields are private and there is no way to build or parse a finding that
/// breaks [`Finding::validate`]: the builder and `Deserialize` both go through
/// `FindingRepr`. Read the members with the getters; change one by rebuilding
/// through [`Finding::builder`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", try_from = "FindingRepr")]
pub struct Finding {
    /// Stable identifier `category.detector`.
    id: FindingId,
    /// Category the finding belongs to.
    category: Category,
    /// Short human title.
    title: String,
    /// Longer explanation of what the data is.
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// Risk level.
    risk: Risk,
    /// Bytes that removing the finding would reclaim. `0` when macOS reports no size.
    reclaimable_bytes: u64,
    /// Number of items behind the finding, when the detector counts them.
    #[serde(skip_serializing_if = "Option::is_none")]
    item_count: Option<u64>,
    /// `false` when Broza cannot remove the finding; the GUI must disable the control.
    actionable: bool,
    /// Action Broza proposes.
    action: Action,
    /// Why the detector reached this conclusion (`--explain`).
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<String>,
    /// Paths behind the finding.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    paths: Vec<FindingPath>,
    /// Snapshots behind the finding. Only for [`Category::Snapshots`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    snapshots: Vec<Snapshot>,
    /// Provider steps. Required for, and only for, [`Action::InformOnly`].
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<Instructions>,
}

/// Wire shape of a [`Finding`]: the only way into one, from JSON or from the builder.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct FindingRepr {
    /// See [`Finding::id`].
    pub(crate) id: FindingId,
    /// See [`Finding::category`].
    pub(crate) category: Category,
    /// See [`Finding::title`].
    pub(crate) title: String,
    /// See [`Finding::description`].
    #[serde(default)]
    pub(crate) description: Option<String>,
    /// See [`Finding::risk`].
    pub(crate) risk: Risk,
    /// See [`Finding::reclaimable_bytes`].
    #[serde(default)]
    pub(crate) reclaimable_bytes: u64,
    /// See [`Finding::item_count`].
    #[serde(default)]
    pub(crate) item_count: Option<u64>,
    /// See `Finding::is_actionable`.
    pub(crate) actionable: bool,
    /// See [`Finding::action`].
    pub(crate) action: Action,
    /// See [`Finding::reasoning`].
    #[serde(default)]
    pub(crate) reasoning: Option<String>,
    /// See [`Finding::paths`].
    #[serde(default)]
    pub(crate) paths: Vec<FindingPath>,
    /// See [`Finding::snapshots`].
    #[serde(default)]
    pub(crate) snapshots: Vec<Snapshot>,
    /// See [`Finding::instructions`].
    #[serde(default)]
    pub(crate) instructions: Option<Instructions>,
}

impl TryFrom<FindingRepr> for Finding {
    type Error = BrozaError;

    fn try_from(repr: FindingRepr) -> Result<Self, Self::Error> {
        let finding = Self {
            id: repr.id,
            category: repr.category,
            title: repr.title,
            description: repr.description,
            risk: repr.risk,
            reclaimable_bytes: repr.reclaimable_bytes,
            item_count: repr.item_count,
            actionable: repr.actionable,
            action: repr.action,
            reasoning: repr.reasoning,
            paths: repr.paths,
            snapshots: repr.snapshots,
            instructions: repr.instructions,
        };
        finding.validate()?;
        Ok(finding)
    }
}

impl Finding {
    /// Start building a finding with the defaults of its category.
    pub fn builder(id: FindingId, category: Category, title: impl Into<String>) -> FindingBuilder {
        FindingBuilder::new(id, category, title)
    }

    /// Stable identifier `category.detector`.
    pub fn id(&self) -> &FindingId {
        &self.id
    }

    /// Category the finding belongs to.
    pub fn category(&self) -> Category {
        self.category
    }

    /// Short human title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Longer explanation of what the data is.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Risk level.
    pub fn risk(&self) -> Risk {
        self.risk
    }

    /// Bytes that removing the finding would reclaim.
    pub fn reclaimable_bytes(&self) -> u64 {
        self.reclaimable_bytes
    }

    /// Number of items behind the finding, when the detector counts them.
    pub fn item_count(&self) -> Option<u64> {
        self.item_count
    }

    /// `false` when Broza cannot remove the finding.
    pub fn is_actionable(&self) -> bool {
        self.actionable
    }

    /// Action Broza proposes.
    pub fn action(&self) -> Action {
        self.action
    }

    /// Why the detector reached this conclusion.
    pub fn reasoning(&self) -> Option<&str> {
        self.reasoning.as_deref()
    }

    /// Paths behind the finding.
    pub fn paths(&self) -> &[FindingPath] {
        &self.paths
    }

    /// Snapshots behind the finding.
    pub fn snapshots(&self) -> &[Snapshot] {
        &self.snapshots
    }

    /// The provider's official steps, for inform-only findings.
    pub fn instructions(&self) -> Option<&Instructions> {
        self.instructions.as_ref()
    }

    /// Check the invariants of `docs/cli-spec.md` §3.3 and §4.3.
    ///
    /// 1. The identifier's category part matches `category`.
    /// 2. [`Category::CloudSynced`] implies `inform_only`, not actionable and `red`.
    /// 3. `actionable` is exactly `action != inform_only`.
    /// 4. `instructions` are present exactly when the action is `inform_only`:
    ///    a finding Broza refuses to act on must tell the user what to do instead.
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
        if self.instructions.is_some() != (self.action == Action::InformOnly) {
            return inconsistent("`instructions` are required by, and limited to, inform-only findings");
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

    pub(crate) fn instructions() -> Instructions {
        Instructions {
            provider: "iCloud Drive".into(),
            summary: "Use Apple's official feature to release local copies.".into(),
            steps: vec!["System Settings".into()],
        }
    }

    fn finding(id: &str, category: Category) -> Finding {
        let builder = Finding::builder(id.parse().unwrap_or_else(|e| panic!("{e}")), category, "title");
        let builder = if category.is_inform_only() { builder.instructions(instructions()) } else { builder };
        builder.build().unwrap_or_else(|e| panic!("{e}"))
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
            assert_eq!(built.risk(), category.base_risk());
            assert_eq!(built.action(), category.default_action());
            assert_eq!(built.is_actionable(), category.default_action().is_actionable());
            assert_eq!(built.category(), category);
            assert_eq!(built.title(), "title");
            assert!(built.validate().is_ok(), "{id}");
        }
    }

    #[test]
    fn the_getters_expose_every_member() {
        let built = Finding::builder(
            "snapshots.timemachine-local".parse().unwrap_or_else(|e| panic!("{e}")),
            Category::Snapshots,
            "Time Machine local snapshots",
        )
        .description("APFS copy-on-write snapshots.")
        .reasoning("size not reported by macOS")
        .reclaimable_bytes(7)
        .item_count(4)
        .paths(vec![FindingPath { path: "/x".into(), size_bytes: 7, last_used: None }])
        .build()
        .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(built.id().as_str(), "snapshots.timemachine-local");
        assert_eq!(built.description(), Some("APFS copy-on-write snapshots."));
        assert_eq!(built.reasoning(), Some("size not reported by macOS"));
        assert_eq!(built.reclaimable_bytes(), 7);
        assert_eq!(built.item_count(), Some(4));
        assert_eq!(built.paths().len(), 1);
        assert!(built.snapshots().is_empty());
        assert!(built.instructions().is_none());
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
        let bad = "{\"path\":\"/x\",\"size_bytes\":0,\"last_used\":\"nope\"}";
        assert!(serde_json::from_str::<FindingPath>(bad).is_err());
    }
}
