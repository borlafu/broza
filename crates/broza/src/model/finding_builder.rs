//! Builder that can only produce findings satisfying [`Finding::validate`].

use crate::BrozaError;
use crate::model::category::Category;
use crate::model::disk::Snapshot;
use crate::model::finding::{Action, Finding, FindingPath, FindingRepr, Instructions, Risk};
use crate::model::ids::FindingId;

/// Incremental, immutable builder for a [`Finding`].
///
/// Risk and action default to [`Category::base_risk`] and
/// [`Category::default_action`]. Setting them explicitly on an inform-only category
/// is an error rather than a silent coercion: a detector that believes it may delete
/// cloud-synced data has a bug, and `AGENTS.md` §2.5 admits no exception.
///
/// `actionable` is always derived from the action, never set by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingBuilder {
    /// Stable identifier.
    id: FindingId,
    /// Category of the finding.
    category: Category,
    /// Short human title.
    title: String,
    /// Longer explanation.
    description: Option<String>,
    /// Risk, when the caller set one.
    risk: Option<Risk>,
    /// Reclaimable bytes.
    reclaimable_bytes: u64,
    /// Number of items behind the finding.
    item_count: Option<u64>,
    /// Action, when the caller set one.
    action: Option<Action>,
    /// Explanation shown by `--explain`.
    reasoning: Option<String>,
    /// Paths behind the finding.
    paths: Vec<FindingPath>,
    /// Snapshots behind the finding.
    snapshots: Vec<Snapshot>,
    /// Provider steps.
    instructions: Option<Instructions>,
}

impl FindingBuilder {
    /// Start from the defaults of `category`.
    pub fn new(id: FindingId, category: Category, title: impl Into<String>) -> Self {
        Self {
            id,
            category,
            title: title.into(),
            description: None,
            risk: None,
            reclaimable_bytes: 0,
            item_count: None,
            action: None,
            reasoning: None,
            paths: Vec::new(),
            snapshots: Vec::new(),
            instructions: None,
        }
    }

    /// Set the long description.
    #[must_use]
    pub fn description(self, description: impl Into<String>) -> Self {
        Self { description: Some(description.into()), ..self }
    }

    /// Raise or lower the risk away from [`Category::base_risk`].
    #[must_use]
    pub fn risk(self, risk: Risk) -> Self {
        Self { risk: Some(risk), ..self }
    }

    /// Set the proposed action, overriding [`Category::default_action`].
    #[must_use]
    pub fn action(self, action: Action) -> Self {
        Self { action: Some(action), ..self }
    }

    /// Set the reclaimable size in bytes.
    #[must_use]
    pub fn reclaimable_bytes(self, reclaimable_bytes: u64) -> Self {
        Self { reclaimable_bytes, ..self }
    }

    /// Set the number of items behind the finding.
    #[must_use]
    pub fn item_count(self, item_count: u64) -> Self {
        Self { item_count: Some(item_count), ..self }
    }

    /// Set the explanation shown by `--explain`.
    #[must_use]
    pub fn reasoning(self, reasoning: impl Into<String>) -> Self {
        Self { reasoning: Some(reasoning.into()), ..self }
    }

    /// Replace the paths behind the finding.
    #[must_use]
    pub fn paths(self, paths: Vec<FindingPath>) -> Self {
        Self { paths, ..self }
    }

    /// Replace the snapshots behind the finding.
    #[must_use]
    pub fn snapshots(self, snapshots: Vec<Snapshot>) -> Self {
        Self { snapshots, ..self }
    }

    /// Attach the provider's official steps. Required for inform-only findings.
    #[must_use]
    pub fn instructions(self, instructions: Instructions) -> Self {
        Self { instructions: Some(instructions), ..self }
    }

    /// Resolve the defaults and validate the result.
    pub fn build(self) -> Result<Finding, BrozaError> {
        let category = self.category;
        let refused = |reason: &str| BrozaError::Other(format!("finding `{}`: {reason}", self.id));
        if category.is_inform_only() {
            if self.action.is_some_and(|action| action != Action::InformOnly) {
                return Err(refused("an inform-only category cannot be given another action"));
            }
            if self.risk.is_some_and(|risk| risk != Risk::Red) {
                return Err(refused("an inform-only category is always red"));
            }
        }
        let action = if category.is_inform_only() {
            Action::InformOnly
        } else {
            self.action.unwrap_or(category.default_action())
        };
        let risk =
            if category.is_inform_only() { Risk::Red } else { self.risk.unwrap_or(category.base_risk()) };
        Finding::try_from(FindingRepr {
            id: self.id,
            category,
            title: self.title,
            description: self.description,
            risk,
            reclaimable_bytes: self.reclaimable_bytes,
            item_count: self.item_count,
            actionable: action.is_actionable(),
            action,
            reasoning: self.reasoning,
            paths: self.paths,
            snapshots: self.snapshots,
            instructions: self.instructions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::FindingBuilder;
    use crate::model::category::Category;
    use crate::model::disk::Snapshot;
    use crate::model::finding::{Action, Finding, FindingPath, Instructions, Risk};
    use crate::model::ids::FindingId;

    fn id(raw: &str) -> FindingId {
        raw.parse().unwrap_or_else(|e| panic!("{raw}: {e}"))
    }

    fn instructions() -> Instructions {
        Instructions {
            provider: "iCloud Drive".into(),
            summary: "Use Apple's official feature to release local copies.".into(),
            steps: vec!["System Settings".into()],
        }
    }

    fn cloud_builder() -> FindingBuilder {
        FindingBuilder::new(id("cloud-synced.icloud"), Category::CloudSynced, "iCloud")
            .instructions(instructions())
    }

    #[test]
    fn a_cloud_synced_finding_defaults_to_red_and_inform_only() {
        let finding = cloud_builder().build().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(finding.action(), Action::InformOnly);
        assert_eq!(finding.risk(), Risk::Red);
        assert!(!finding.is_actionable());
        assert!(finding.instructions().is_some());
    }

    #[test]
    fn a_cloud_synced_finding_refuses_another_action_or_risk() {
        for action in [Action::Quarantine, Action::Purge, Action::TmutilDelete] {
            assert!(cloud_builder().action(action).build().is_err(), "{action:?}");
        }
        for risk in [Risk::Green, Risk::Amber] {
            assert!(cloud_builder().risk(risk).build().is_err(), "{risk:?}");
        }
        assert!(cloud_builder().action(Action::InformOnly).risk(Risk::Red).build().is_ok());
    }

    #[test]
    fn actionable_always_mirrors_the_action() {
        let cases = [(Action::Quarantine, true), (Action::Purge, true), (Action::TmutilDelete, true)];
        for (action, expected) in cases {
            let finding = FindingBuilder::new(id("build-cache.docker-raw"), Category::BuildCache, "Docker")
                .action(action)
                .build()
                .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(finding.is_actionable(), expected, "{action:?}");
        }
        let inform = FindingBuilder::new(id("build-cache.docker-raw"), Category::BuildCache, "Docker")
            .action(Action::InformOnly)
            .instructions(instructions())
            .build()
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(!inform.is_actionable());
    }

    #[test]
    fn instructions_are_required_by_and_limited_to_inform_only_findings() {
        let stray = FindingBuilder::new(id("user-cache.logs"), Category::UserCache, "Logs")
            .instructions(instructions())
            .build();
        assert!(stray.is_err());

        let missing = FindingBuilder::new(id("build-cache.docker-raw"), Category::BuildCache, "Docker")
            .action(Action::InformOnly)
            .build();
        assert!(missing.is_err());
    }

    #[test]
    fn snapshots_outside_the_snapshots_category_are_rejected() {
        let built = FindingBuilder::new(id("user-cache.logs"), Category::UserCache, "Logs")
            .snapshots(vec![Snapshot {
                name: "s".into(),
                uuid: None,
                purgeable: true,
                volume: None,
                mount_point: None,
            }])
            .build();
        assert!(built.is_err());
    }

    #[test]
    fn an_identifier_from_another_category_is_rejected() {
        let built = FindingBuilder::new(id("trash.user"), Category::UserCache, "Logs").build();
        assert!(built.is_err());
    }

    #[test]
    fn optional_members_are_carried_through_and_the_builder_does_not_mutate() {
        let base = FindingBuilder::new(id("snapshots.timemachine-local"), Category::Snapshots, "Snapshots");
        let enriched = base
            .clone()
            .description("APFS copy-on-write snapshots.")
            .reasoning("size not reported by macOS")
            .reclaimable_bytes(0)
            .item_count(4)
            .paths(vec![FindingPath { path: "/x".into(), size_bytes: 1, last_used: None }])
            .snapshots(vec![Snapshot {
                name: "s".into(),
                uuid: Some("u".into()),
                purgeable: true,
                volume: None,
                mount_point: None,
            }])
            .build()
            .unwrap_or_else(|e| panic!("{e}"));
        let plain: Finding = base.build().unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(enriched.description(), Some("APFS copy-on-write snapshots."));
        assert_eq!(enriched.item_count(), Some(4));
        assert_eq!(enriched.paths().len(), 1);
        assert_eq!(enriched.snapshots().len(), 1);
        assert_eq!(enriched.action(), Action::TmutilDelete);
        assert!(plain.description().is_none(), "the shared builder was mutated");
        assert!(plain.snapshots().is_empty(), "the shared builder was mutated");
    }

    #[test]
    fn a_finding_can_only_be_parsed_when_it_is_consistent() {
        let valid = serde_json::json!({
            "id": "user-cache.logs",
            "category": "user-cache",
            "title": "Logs",
            "risk": "green",
            "reclaimable_bytes": 10,
            "actionable": true,
            "action": "quarantine"
        });
        assert!(serde_json::from_value::<Finding>(valid.clone()).is_ok());

        let mut lying = valid.clone();
        lying["actionable"] = serde_json::json!(false);
        assert!(serde_json::from_value::<Finding>(lying).is_err());

        let mut stray_category = valid;
        stray_category["category"] = serde_json::json!("trash");
        assert!(serde_json::from_value::<Finding>(stray_category).is_err());
    }
}
