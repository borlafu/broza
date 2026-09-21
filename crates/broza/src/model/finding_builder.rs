//! Builder that can only produce findings satisfying [`Finding::validate`].

use crate::BrozaError;
use crate::model::category::Category;
use crate::model::disk::Snapshot;
use crate::model::finding::{Action, Finding, FindingPath, Instructions, Risk};
use crate::model::ids::FindingId;

/// Incremental, immutable builder for a [`Finding`].
///
/// The builder starts from the category defaults ([`Category::base_risk`] and
/// [`Category::default_action`]) and coerces the invariants that the specification
/// states unconditionally:
///
/// - [`Category::CloudSynced`] is always `red`, `inform_only` and not actionable
///   (`AGENTS.md` §2.5); an attempt to set another action or risk is ignored.
/// - `actionable` is always derived, never set by the caller.
///
/// Combinations that would silently lose data ([`Instructions`] on an actionable
/// finding, snapshots on another category, an identifier that disagrees with the
/// category) are rejected by [`FindingBuilder::build`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingBuilder {
    /// Finding assembled so far.
    finding: Finding,
}

impl FindingBuilder {
    /// Start from the defaults of `category`.
    pub fn new(id: FindingId, category: Category, title: impl Into<String>) -> Self {
        let action = category.default_action();
        Self {
            finding: Finding {
                id,
                category,
                title: title.into(),
                description: None,
                risk: category.base_risk(),
                reclaimable_bytes: 0,
                item_count: None,
                actionable: action.is_actionable(),
                action,
                reasoning: None,
                paths: Vec::new(),
                snapshots: Vec::new(),
                instructions: None,
            },
        }
    }

    /// Set the long description.
    #[must_use]
    pub fn description(self, description: impl Into<String>) -> Self {
        Self { finding: Finding { description: Some(description.into()), ..self.finding } }
    }

    /// Raise or lower the risk. Ignored for inform-only categories.
    #[must_use]
    pub fn risk(self, risk: Risk) -> Self {
        Self { finding: Finding { risk, ..self.finding } }
    }

    /// Set the proposed action. Ignored for inform-only categories.
    #[must_use]
    pub fn action(self, action: Action) -> Self {
        Self { finding: Finding { action, ..self.finding } }
    }

    /// Set the reclaimable size in bytes.
    #[must_use]
    pub fn reclaimable_bytes(self, reclaimable_bytes: u64) -> Self {
        Self { finding: Finding { reclaimable_bytes, ..self.finding } }
    }

    /// Set the number of items behind the finding.
    #[must_use]
    pub fn item_count(self, item_count: u64) -> Self {
        Self { finding: Finding { item_count: Some(item_count), ..self.finding } }
    }

    /// Set the explanation shown by `--explain`.
    #[must_use]
    pub fn reasoning(self, reasoning: impl Into<String>) -> Self {
        Self { finding: Finding { reasoning: Some(reasoning.into()), ..self.finding } }
    }

    /// Replace the paths behind the finding.
    #[must_use]
    pub fn paths(self, paths: Vec<FindingPath>) -> Self {
        Self { finding: Finding { paths, ..self.finding } }
    }

    /// Replace the snapshots behind the finding.
    #[must_use]
    pub fn snapshots(self, snapshots: Vec<Snapshot>) -> Self {
        Self { finding: Finding { snapshots, ..self.finding } }
    }

    /// Attach the provider's official steps. Only valid for inform-only findings.
    #[must_use]
    pub fn instructions(self, instructions: Instructions) -> Self {
        Self { finding: Finding { instructions: Some(instructions), ..self.finding } }
    }

    /// Apply the unconditional coercions and validate the result.
    pub fn build(self) -> Result<Finding, BrozaError> {
        let category = self.finding.category;
        let action = if category.is_inform_only() { Action::InformOnly } else { self.finding.action };
        let risk = if category.is_inform_only() { Risk::Red } else { self.finding.risk };
        let finding = Finding { action, risk, actionable: action.is_actionable(), ..self.finding };
        finding.validate()?;
        Ok(finding)
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

    #[test]
    fn cloud_synced_findings_are_forced_to_inform_only() {
        let finding = FindingBuilder::new(id("cloud-synced.icloud"), Category::CloudSynced, "iCloud")
            .action(Action::Purge)
            .risk(Risk::Green)
            .instructions(instructions())
            .build()
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(finding.action, Action::InformOnly);
        assert_eq!(finding.risk, Risk::Red);
        assert!(!finding.actionable);
        assert!(finding.instructions.is_some());
    }

    #[test]
    fn actionable_always_mirrors_the_action() {
        let cases = [
            (Action::Quarantine, true),
            (Action::Purge, true),
            (Action::TmutilDelete, true),
            (Action::InformOnly, false),
        ];
        for (action, expected) in cases {
            let finding = FindingBuilder::new(id("build-cache.docker-raw"), Category::BuildCache, "Docker")
                .action(action)
                .build()
                .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(finding.actionable, expected, "{action:?}");
        }
    }

    #[test]
    fn instructions_on_an_actionable_finding_are_rejected() {
        let built = FindingBuilder::new(id("user-cache.logs"), Category::UserCache, "Logs")
            .instructions(instructions())
            .build();
        assert!(built.is_err());
    }

    #[test]
    fn snapshots_outside_the_snapshots_category_are_rejected() {
        let built = FindingBuilder::new(id("user-cache.logs"), Category::UserCache, "Logs")
            .snapshots(vec![Snapshot { name: "s".into(), uuid: None, purgeable: true }])
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
            .snapshots(vec![Snapshot { name: "s".into(), uuid: Some("u".into()), purgeable: true }])
            .build()
            .unwrap_or_else(|e| panic!("{e}"));
        let plain: Finding = base.build().unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(enriched.description.as_deref(), Some("APFS copy-on-write snapshots."));
        assert_eq!(enriched.reasoning.as_deref(), Some("size not reported by macOS"));
        assert_eq!(enriched.item_count, Some(4));
        assert_eq!(enriched.paths.len(), 1);
        assert_eq!(enriched.snapshots.len(), 1);
        assert_eq!(enriched.action, Action::TmutilDelete);
        assert!(plain.description.is_none(), "the shared builder was mutated");
        assert!(plain.snapshots.is_empty(), "the shared builder was mutated");
    }
}
