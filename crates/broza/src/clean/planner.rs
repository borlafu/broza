//! Dry-run planner: findings plus a selection become a [`CleanPlan`].
//!
//! Pure function, no I/O. The plan it returns is always a dry run; `--apply` only
//! changes what the safety kernel and the executor do with it
//! (`docs/cli-spec.md` §3.4, "Execution order").

use std::path::Path;

use crate::BrozaError;
use crate::model::{
    Action, Category, CleanItem, CleanPlan, Finding, FindingId, ItemStatus, Risk, SessionId, SnapshotRef,
};
use crate::safety::exclusions::Exclusions;
use crate::safety::guard::expected_action;

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

/// The plan, plus what the selection matched but Broza will not clean.
///
/// `informed_only` is not an error: a dry run reports those findings and exits
/// `0`. It becomes one under `--apply`, where the guard refuses the whole plan
/// (`docs/cli-spec.md` §2): the user asked for a category Broza only ever
/// reports (`cloud-synced`). `informed_in_passing` never rejects anything: an
/// inform-only finding that happens to live inside an actionable category
/// (`build-cache.docker-raw`) is left out of the plan and named in a warning,
/// so `clean --category build-cache --apply` works on a machine with Docker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanOutcome {
    /// The dry-run plan.
    pub plan: CleanPlan,
    /// Findings of an inform-only category the selection asked for (`cloud-synced`).
    pub informed_only: Vec<FindingId>,
    /// Inform-only findings inside actionable categories, left out of the plan.
    pub informed_in_passing: Vec<FindingId>,
}

/// Why a plan could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// The resulting plan would break an invariant of the model.
    #[error("{0}")]
    Invalid(String),
}

impl From<PlanError> for BrozaError {
    fn from(error: PlanError) -> Self {
        match error {
            PlanError::Invalid(reason) => Self::Other(reason),
        }
    }
}

/// Highest risk among the selected, actionable findings.
///
/// Informational only: the guard derives the risk it enforces from the findings
/// the plan refers to, never from a value a caller passes in.
pub fn max_risk(findings: &[Finding], selection: &Selection) -> Option<Risk> {
    findings
        .iter()
        .filter(|finding| selection.matches(finding) && finding.action() != Action::InformOnly)
        .map(Finding::risk)
        .max()
}

/// Builds the dry-run plan for a selection.
///
/// One [`CleanItem`] per path of every selected finding, in finding order, with
/// `status: planned`. Excluded paths and anything inside `quarantine_root` are
/// dropped before the bytes are summed; `--purge` upgrades
/// [`Action::Quarantine`] to [`Action::Purge`]. Inform-only findings never
/// produce an item; they are returned in [`PlanOutcome::informed_only`] or
/// [`PlanOutcome::informed_in_passing`] depending on their category.
pub fn plan_dry_run(
    findings: &[Finding],
    selection: &Selection,
    session_id: SessionId,
    quarantine_root: Option<&Path>,
) -> Result<PlanOutcome, PlanError> {
    if let Some(root) = quarantine_root {
        validate_quarantine_root(root)?;
    }
    let selected: Vec<&Finding> = findings.iter().filter(|f| selection.matches(f)).collect();
    let informed: Vec<&Finding> =
        selected.iter().copied().filter(|finding| finding.action() == Action::InformOnly).collect();
    let (informed_only, informed_in_passing): (Vec<&Finding>, Vec<&Finding>) =
        informed.iter().partition(|finding| finding.category().is_inform_only());
    let informed_only = informed_only.iter().map(|finding| finding.id().clone()).collect();
    let informed_in_passing = informed_in_passing.iter().map(|finding| finding.id().clone()).collect();
    let items: Vec<CleanItem> = selected
        .iter()
        .filter(|finding| finding.action() != Action::InformOnly)
        .flat_map(|finding| items_of(finding, selection, quarantine_root))
        .collect();
    let plan =
        CleanPlan::dry_run(session_id, items).map_err(|error| PlanError::Invalid(error.to_string()))?;
    Ok(PlanOutcome { plan, informed_only, informed_in_passing })
}

/// The quarantine root is compared as a prefix, so a malformed one would
/// silently protect nothing. The guard checks it again against the mount table;
/// here only its shape can be judged.
fn validate_quarantine_root(root: &Path) -> Result<(), PlanError> {
    if root.is_absolute() && root != Path::new("/") {
        return Ok(());
    }
    Err(PlanError::Invalid(format!(
        "the quarantine root `{}` must be an absolute path inside a volume",
        root.display()
    )))
}

/// The items one finding contributes, minus the excluded paths.
fn items_of(finding: &Finding, selection: &Selection, quarantine_root: Option<&Path>) -> Vec<CleanItem> {
    let action = expected_action(finding.action(), selection.purge);
    if finding.category() == Category::Snapshots {
        return snapshot_items(finding, action);
    }
    finding
        .paths()
        .iter()
        .filter(|candidate| !selection.exclusions.matches(&candidate.path, true))
        .filter(|candidate| quarantine_root.is_none_or(|root| !candidate.path.starts_with(root)))
        .map(|candidate| CleanItem {
            path: candidate.path.clone(),
            finding_id: finding.id().clone(),
            size_bytes: candidate.size_bytes,
            status: ItemStatus::Planned,
            action,
            error: None,
            snapshot: None,
        })
        .collect()
}

/// One item per actionable snapshot of a `snapshots` finding.
///
/// A snapshot the finding could not tie to a mounted volume, or that has no
/// UUID, is left out: the plan item's `path` is that volume's mount point and
/// the guard verifies the pair against the mount table (`docs/cli-spec.md`
/// §4.4). `--exclude` patterns are path-based and do not apply to snapshots.
fn snapshot_items(finding: &Finding, action: Action) -> Vec<CleanItem> {
    finding
        .snapshots()
        .iter()
        .filter(|snapshot| snapshot.is_actionable())
        .filter_map(|snapshot| {
            let volume = snapshot.volume.clone()?;
            let mount_point = snapshot.mount_point.clone()?;
            let uuid = snapshot.uuid.clone()?;
            Some(CleanItem {
                path: mount_point,
                finding_id: finding.id().clone(),
                size_bytes: 0,
                status: ItemStatus::Planned,
                action,
                error: None,
                snapshot: Some(SnapshotRef { volume, name: snapshot.name.clone(), uuid }),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{PlanOutcome, Selection, max_risk, plan_dry_run};
    use crate::model::{Action, Category, Finding, FindingPath, Instructions, ItemStatus, Risk, SessionId};
    use crate::safety::exclusions::Exclusions;
    use std::path::{Path, PathBuf};

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
            .reclaimable_bytes(paths.iter().map(|(_, size)| *size).fold(0_u64, u64::saturating_add))
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

    fn outcome(findings: &[Finding], selection: &Selection) -> PlanOutcome {
        plan_dry_run(findings, selection, session(), None).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn one_item_per_path_all_planned() {
        let plan = outcome(&[caches()], &Selection::everything()).plan;
        assert!(plan.is_dry_run());
        assert_eq!(plan.items().len(), 2);
        assert_eq!(plan.planned_bytes(), 30);
        assert!(plan.items().iter().all(|item| item.status == ItemStatus::Planned));
        assert!(plan.items().iter().all(|item| item.action == Action::Quarantine));
    }

    #[test]
    fn purge_upgrades_quarantine_but_leaves_other_actions_alone() {
        let selection = Selection { purge: true, ..Selection::everything() };
        let plan = outcome(&[caches(), trash()], &selection).plan;
        let actions: Vec<Action> = plan.items().iter().map(|item| item.action).collect();
        assert_eq!(actions, vec![Action::Purge, Action::Purge, Action::Purge]);
    }

    #[test]
    fn the_trash_is_purged_even_without_the_flag() {
        let plan = outcome(&[trash()], &Selection::everything()).plan;
        assert_eq!(plan.items()[0].action, Action::Purge, "emptying the trash is what the trash is");
    }

    #[test]
    fn excluded_paths_are_dropped_before_the_bytes_are_summed() {
        let exclusions =
            Exclusions::new(["/Users/dana/Library/Caches/b"]).unwrap_or_else(|error| panic!("{error}"));
        let selection = Selection { exclusions, ..Selection::everything() };
        let plan = outcome(&[caches()], &selection).plan;
        assert_eq!(plan.items().len(), 1);
        assert_eq!(plan.planned_bytes(), 10);
    }

    #[test]
    fn the_quarantine_store_is_never_cleaned_by_the_plan_that_fills_it() {
        let store = Path::new("/Users/dana/.local/share/broza/quarantine");
        let inside = finding(
            "build-cache.derived",
            Category::BuildCache,
            &[("/Users/dana/.local/share/broza/quarantine/cln_x/items/1", 99)],
        );
        let plan = plan_dry_run(&[inside], &Selection::everything(), session(), Some(store))
            .unwrap_or_else(|error| panic!("{error}"))
            .plan;
        assert_eq!(plan.items().len(), 0);
        assert_eq!(plan.planned_bytes(), 0);
    }

    #[test]
    fn a_category_filter_selects_only_that_category() {
        let selection = Selection { categories: Some(vec![Category::Trash]), ..Selection::everything() };
        let plan = outcome(&[caches(), trash()], &selection).plan;
        assert_eq!(plan.items().len(), 1);
        assert_eq!(plan.items()[0].path, PathBuf::from("/Users/dana/.Trash/x"));
    }

    #[test]
    fn a_risk_ceiling_excludes_riskier_findings() {
        let selection = Selection { risk_ceiling: Some(Risk::Green), ..Selection::everything() };
        let plan = outcome(&[caches(), trash(), cloud_synced()], &selection).plan;
        assert_eq!(plan.items().len(), 2, "only the green user-cache survives");
    }

    #[test]
    fn an_inform_only_finding_is_reported_instead_of_planned() {
        let outcome = outcome(&[caches(), cloud_synced()], &Selection::everything());
        assert_eq!(outcome.plan.items().len(), 2, "only the cache paths are planned");
        assert_eq!(
            outcome.informed_only,
            vec!["cloud-synced.icloud".parse().unwrap_or_else(|e| panic!("{e}"))]
        );
        assert!(
            outcome.plan.items().iter().all(|item| item.action != Action::InformOnly),
            "an inform-only finding never becomes an item"
        );
    }

    #[test]
    fn an_inform_only_finding_inside_an_actionable_category_is_left_out_in_passing() {
        let docker = Finding::builder(
            "build-cache.docker-raw".parse().unwrap_or_else(|e| panic!("{e}")),
            Category::BuildCache,
            "Docker disk",
        )
        .action(Action::InformOnly)
        .instructions(Instructions { provider: "Docker".into(), summary: "prune".into(), steps: Vec::new() })
        .build()
        .unwrap_or_else(|e| panic!("{e}"));
        let selection = Selection { categories: Some(vec![Category::BuildCache]), ..Selection::everything() };

        let outcome = outcome(&[docker, caches()], &selection);

        assert!(outcome.informed_only.is_empty(), "the category itself is actionable");
        assert_eq!(
            outcome.informed_in_passing,
            vec!["build-cache.docker-raw".parse().unwrap_or_else(|e| panic!("{e}"))]
        );
        assert!(outcome.plan.items().is_empty(), "nothing of build-cache is actionable here");
    }

    #[test]
    fn an_empty_selection_is_an_empty_plan() {
        let selection = Selection { categories: Some(vec![Category::Duplicates]), ..Selection::everything() };
        let outcome = outcome(&[caches()], &selection);
        assert_eq!(outcome.plan.items().len(), 0);
        assert_eq!(outcome.plan.planned_bytes(), 0);
        assert!(outcome.informed_only.is_empty());
    }

    #[test]
    fn the_maximum_risk_ignores_what_can_never_be_cleaned() {
        let findings = [caches(), trash(), cloud_synced()];
        assert_eq!(
            max_risk(&findings, &Selection::everything()),
            Some(Risk::Amber),
            "the red cloud-synced finding is inform-only, not a risk to confirm"
        );
        let green = Selection { risk_ceiling: Some(Risk::Green), ..Selection::everything() };
        assert_eq!(max_risk(&findings, &green), Some(Risk::Green));
        let none = Selection { categories: Some(Vec::new()), ..Selection::everything() };
        assert_eq!(max_risk(&findings, &none), None);
    }

    #[test]
    fn a_quarantine_root_that_protects_nothing_is_refused() {
        for root in ["relative/quarantine", "/"] {
            let error = plan_dry_run(&[caches()], &Selection::everything(), session(), Some(Path::new(root)));
            assert!(matches!(error, Err(super::PlanError::Invalid(_))), "{root}: {error:?}");
        }
    }

    #[test]
    fn an_invalid_plan_becomes_a_generic_error() {
        let error = crate::BrozaError::from(super::PlanError::Invalid("overflow".to_owned()));
        assert_eq!(crate::ExitCode::from(&error), crate::ExitCode::GenericError);
    }

    #[test]
    fn item_sizes_that_overflow_are_refused_rather_than_wrapped() {
        let huge = finding(
            "large-old-files.big",
            Category::LargeOldFiles,
            &[("/Users/dana/a", u64::MAX), ("/Users/dana/b", 1)],
        );
        let error = plan_dry_run(&[huge], &Selection::everything(), session(), None);
        assert!(matches!(error, Err(super::PlanError::Invalid(_))), "{error:?}");
    }
}
