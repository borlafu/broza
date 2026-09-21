//! Dry-run planner: findings plus a selection become a [`CleanPlan`].
//!
//! Pure function, no I/O. The plan it returns is always a dry run; `--apply` only
//! changes what the safety kernel and the executor do with it
//! (`docs/cli-spec.md` §3.4, "Execution order").

use std::path::Path;

use crate::BrozaError;
use crate::model::{Action, Category, CleanItem, CleanPlan, Finding, FindingId, ItemStatus, Risk, SessionId};
use crate::safety::exclusions::Exclusions;

/// What the user asked to clean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// `--category`; `None` means "every category the risk ceiling allows".
    pub categories: Option<Vec<Category>>,
    /// `--risk`: clean everything at this level or lower.
    pub risk_ceiling: Option<Risk>,
    /// `--purge`: upgrade quarantine to irreversible deletion.
    pub purge: bool,
    /// Exclusions from the configuration and `--exclude`.
    pub exclusions: Exclusions,
}

impl Selection {
    /// A selection that matches every actionable finding.
    pub fn everything() -> Self {
        Self { categories: None, risk_ceiling: None, purge: false, exclusions: Exclusions::none() }
    }

    /// `true` when this finding's category and risk are inside the selection.
    fn matches(&self, finding: &Finding) -> bool {
        let by_category =
            self.categories.as_ref().is_none_or(|categories| categories.contains(&finding.category()));
        let by_risk = self.risk_ceiling.is_none_or(|ceiling| finding.risk() <= ceiling);
        by_category && by_risk
    }
}

/// Why a plan could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// The selection contains a finding Broza only reports (`cloud-synced`).
    #[error("finding `{0}` is inform-only and can never be cleaned")]
    InformOnlySelected(FindingId),
    /// The resulting plan would break an invariant of the model.
    #[error("{0}")]
    Invalid(String),
}

impl From<PlanError> for BrozaError {
    fn from(error: PlanError) -> Self {
        match error {
            PlanError::InformOnlySelected(_) => Self::Usage(error.to_string()),
            PlanError::Invalid(reason) => Self::Other(reason),
        }
    }
}

/// Highest risk among the selected findings; `None` when nothing is selected.
///
/// This is what the caller passes to [`crate::safety::guard::WriteRequest::max_risk`].
pub fn max_risk(findings: &[Finding], selection: &Selection) -> Option<Risk> {
    findings.iter().filter(|finding| selection.matches(finding)).map(Finding::risk).max()
}

/// Builds the dry-run plan for a selection.
///
/// One [`CleanItem`] per path of every selected finding, in finding order, with
/// `status: planned`. Excluded paths and anything inside `quarantine_root` are
/// dropped before the bytes are summed; `--purge` upgrades
/// [`Action::Quarantine`] to [`Action::Purge`]. An `inform_only` finding inside
/// the selection is an error: the whole plan is refused.
pub fn plan_dry_run(
    findings: &[Finding],
    selection: &Selection,
    session_id: SessionId,
    quarantine_root: Option<&Path>,
) -> Result<CleanPlan, PlanError> {
    let selected: Vec<&Finding> = findings.iter().filter(|f| selection.matches(f)).collect();
    if let Some(finding) = selected.iter().find(|f| f.action() == Action::InformOnly) {
        return Err(PlanError::InformOnlySelected(finding.id().clone()));
    }
    let items: Vec<CleanItem> =
        selected.iter().flat_map(|finding| items_of(finding, selection, quarantine_root)).collect();
    CleanPlan::dry_run(session_id, items).map_err(|error| PlanError::Invalid(error.to_string()))
}

/// The items one finding contributes, minus the excluded paths.
fn items_of(finding: &Finding, selection: &Selection, quarantine_root: Option<&Path>) -> Vec<CleanItem> {
    let action = effective_action(finding.action(), selection.purge);
    finding
        .paths()
        .iter()
        .filter(|candidate| !selection.exclusions.matches(&candidate.path))
        .filter(|candidate| quarantine_root.is_none_or(|root| !candidate.path.starts_with(root)))
        .map(|candidate| CleanItem {
            path: candidate.path.clone(),
            finding_id: finding.id().clone(),
            size_bytes: candidate.size_bytes,
            status: ItemStatus::Planned,
            action,
            error: None,
        })
        .collect()
}

/// `--purge` turns a quarantine into an irreversible deletion; nothing else changes.
fn effective_action(action: Action, purge: bool) -> Action {
    if purge && action == Action::Quarantine { Action::Purge } else { action }
}

#[cfg(test)]
mod tests {
    use super::{PlanError, Selection, max_risk, plan_dry_run};
    use crate::model::{Action, Category, Finding, FindingPath, Instructions, ItemStatus, Risk, SessionId};
    use crate::safety::exclusions::Exclusions;
    use std::path::PathBuf;

    fn session() -> SessionId {
        "cln_20260921103608_a1b2".parse().unwrap_or_else(|error| panic!("{error}"))
    }

    fn finding(id: &str, category: Category, paths: &[(&str, u64)]) -> Finding {
        let id = id.parse().unwrap_or_else(|error| panic!("{error}"));
        Finding::builder(id, category, "title")
            .paths(
                paths
                    .iter()
                    .map(|(path, size)| FindingPath {
                        path: PathBuf::from(path),
                        size_bytes: *size,
                        last_used: None,
                    })
                    .collect(),
            )
            .reclaimable_bytes(paths.iter().map(|(_, size)| size).sum())
            .build()
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn cloud_synced() -> Finding {
        Finding::builder(
            "cloud-synced.icloud".parse().unwrap_or_else(|e| panic!("{e}")),
            Category::CloudSynced,
            "iCloud",
        )
        .paths(vec![FindingPath {
            path: "/Users/dana/Library/Mobile Documents/a".into(),
            size_bytes: 9,
            last_used: None,
        }])
        .instructions(Instructions {
            provider: "iCloud Drive".into(),
            summary: "Use Apple's own feature.".into(),
            steps: Vec::new(),
        })
        .build()
        .unwrap_or_else(|error| panic!("{error}"))
    }

    fn caches() -> Finding {
        finding(
            "user-cache.app",
            Category::UserCache,
            &[("/Users/dana/Library/Caches/a", 10), ("/Users/dana/Library/Caches/b", 20)],
        )
    }

    fn trash() -> Finding {
        finding("trash.volumes", Category::Trash, &[("/Users/dana/.Trash/x", 5)])
    }

    fn plan(findings: &[Finding], selection: &Selection) -> crate::model::CleanPlan {
        plan_dry_run(findings, selection, session(), None).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn one_item_per_path_all_planned() {
        let plan = plan(&[caches()], &Selection::everything());
        assert!(plan.is_dry_run());
        assert_eq!(plan.items().len(), 2);
        assert_eq!(plan.planned_bytes(), 30);
        assert!(plan.items().iter().all(|item| item.status == ItemStatus::Planned));
        assert!(plan.items().iter().all(|item| item.action == Action::Quarantine));
    }

    #[test]
    fn purge_upgrades_quarantine_but_leaves_other_actions_alone() {
        let selection = Selection { purge: true, ..Selection::everything() };
        let plan = plan(&[caches(), trash()], &selection);
        let actions: Vec<Action> = plan.items().iter().map(|item| item.action).collect();
        assert_eq!(actions, vec![Action::Purge, Action::Purge, Action::Purge]);
    }

    #[test]
    fn excluded_paths_are_dropped_before_the_bytes_are_summed() {
        let exclusions =
            Exclusions::new(["/Users/dana/Library/Caches/b"]).unwrap_or_else(|error| panic!("{error}"));
        let selection = Selection { exclusions, ..Selection::everything() };
        let plan = plan(&[caches()], &selection);
        assert_eq!(plan.items().len(), 1);
        assert_eq!(plan.planned_bytes(), 10);
    }

    #[test]
    fn the_quarantine_store_is_never_cleaned_by_the_plan_that_fills_it() {
        let store = PathBuf::from("/Users/dana/.local/share/broza/quarantine");
        let inside = finding(
            "build-cache.derived",
            Category::BuildCache,
            &[("/Users/dana/.local/share/broza/quarantine/cln_x/items/1", 99)],
        );
        let plan = plan_dry_run(&[inside], &Selection::everything(), session(), Some(&store))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(plan.items().len(), 0);
        assert_eq!(plan.planned_bytes(), 0);
    }

    #[test]
    fn a_category_filter_selects_only_that_category() {
        let selection = Selection { categories: Some(vec![Category::Trash]), ..Selection::everything() };
        let plan = plan(&[caches(), trash()], &selection);
        assert_eq!(plan.items().len(), 1);
        assert_eq!(plan.items()[0].path, PathBuf::from("/Users/dana/.Trash/x"));
    }

    #[test]
    fn a_risk_ceiling_excludes_riskier_findings() {
        let selection = Selection { risk_ceiling: Some(Risk::Green), ..Selection::everything() };
        let plan = plan(&[caches(), trash(), cloud_synced()], &selection);
        assert_eq!(plan.items().len(), 2, "only the green user-cache survives");
    }

    #[test]
    fn an_inform_only_finding_in_the_selection_rejects_the_plan() {
        let error = plan_dry_run(&[caches(), cloud_synced()], &Selection::everything(), session(), None);
        assert_eq!(
            error,
            Err(PlanError::InformOnlySelected(
                "cloud-synced.icloud".parse().unwrap_or_else(|e| panic!("{e}"))
            ))
        );
    }

    #[test]
    fn an_empty_selection_is_an_empty_plan() {
        let selection = Selection { categories: Some(vec![Category::Duplicates]), ..Selection::everything() };
        let plan = plan(&[caches()], &selection);
        assert_eq!(plan.items().len(), 0);
        assert_eq!(plan.planned_bytes(), 0);
    }

    #[test]
    fn the_maximum_risk_follows_the_selection() {
        let findings = [caches(), trash(), cloud_synced()];
        assert_eq!(max_risk(&findings, &Selection::everything()), Some(Risk::Red));
        let green = Selection { risk_ceiling: Some(Risk::Green), ..Selection::everything() };
        assert_eq!(max_risk(&findings, &green), Some(Risk::Green));
        let none = Selection { categories: Some(Vec::new()), ..Selection::everything() };
        assert_eq!(max_risk(&findings, &none), None);
    }

    #[test]
    fn a_plan_error_becomes_a_usage_error() {
        let error = crate::BrozaError::from(PlanError::InformOnlySelected(
            "cloud-synced.icloud".parse().unwrap_or_else(|e| panic!("{e}")),
        ));
        assert_eq!(crate::ExitCode::from(&error), crate::ExitCode::UsageError);
        let invalid = crate::BrozaError::from(PlanError::Invalid("overflow".to_owned()));
        assert_eq!(crate::ExitCode::from(&invalid), crate::ExitCode::GenericError);
    }
}
