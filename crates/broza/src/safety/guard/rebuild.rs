//! Turning the per-item outcomes back into a plan.
//!
//! The rebuilt plan is what the executor will act on: every path is one the guard
//! checked, every vanished item is `skipped` with `not_found`, and
//! `planned_bytes` counts only what will actually be removed.

use super::ApprovedPlan;
use super::item::{Outcome, OutcomeKind};
use crate::model::{Action, CleanItem, CleanPlan, CleanPlanRepr, ItemErrorCode, ItemStatus};
use crate::safety::rejection::GuardRejection;

/// Rebuilds the plan from the checked paths, marking the vanished ones `skipped`.
///
/// The result is no longer a dry run: it is the plan that will be applied, it
/// carries only paths the guard validated, and `planned_bytes` counts only what
/// will actually be removed, because a skipped item is worth zero bytes.
pub(super) fn rebuild(plan: &CleanPlan, outcomes: &[Outcome]) -> Result<ApprovedPlan, GuardRejection> {
    let mut checked = outcomes.iter();
    let items = plan
        .items()
        .iter()
        .enumerate()
        .map(|(index, item)| match item.action {
            Action::InformOnly => Ok(item.clone()),
            _ => match checked.next() {
                Some(outcome) if outcome.index == index => Ok(rebuilt_item(item, &outcome.kind)),
                _ => Err(desynchronised(item)),
            },
        })
        .collect::<Result<Vec<CleanItem>, GuardRejection>>()?;
    if checked.next().is_some() {
        return Err(GuardRejection::Inconsistent("more checked items than the plan has".to_owned()));
    }
    let planned_bytes = items
        .iter()
        .try_fold(0_u64, |sum, item| sum.checked_add(item.size_bytes))
        .ok_or_else(|| GuardRejection::Inconsistent("the item sizes overflow u64".to_owned()))?;
    let repr = CleanPlanRepr {
        dry_run: false,
        session_id: plan.session_id().clone(),
        planned_bytes,
        quarantined_bytes: 0,
        reclaimed_bytes: 0,
        quarantine_path: None,
        expired_sessions: Vec::new(),
        items,
    };
    let plan = CleanPlan::new(repr)
        .map_err(|error| GuardRejection::Inconsistent(format!("approved plan: {error}")))?;
    let items = outcomes
        .iter()
        .filter_map(|outcome| match &outcome.kind {
            OutcomeKind::Approved(item) => Some(item.clone()),
            OutcomeKind::Missing => None,
        })
        .collect();
    Ok(ApprovedPlan::new(plan, items))
}

fn desynchronised(item: &CleanItem) -> GuardRejection {
    GuardRejection::Inconsistent(format!("no check result for item `{}`", item.path.display()))
}

/// One item of the approved plan: the checked path, the observed size, or nothing.
fn rebuilt_item(item: &CleanItem, kind: &OutcomeKind) -> CleanItem {
    match kind {
        OutcomeKind::Approved(approved) => CleanItem {
            path: approved.path().to_path_buf(),
            // A directory's size was aggregated by the scanner; `lstat` only sees
            // the directory entry, so the scanned figure is the useful one.
            size_bytes: if approved.is_dir() { item.size_bytes } else { approved.size_bytes() },
            ..item.clone()
        },
        OutcomeKind::Missing => CleanItem {
            status: ItemStatus::Skipped,
            error: Some(ItemErrorCode::NotFound),
            size_bytes: 0,
            ..item.clone()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{Outcome, OutcomeKind, rebuild};
    use crate::model::{Action, CleanItem, CleanPlan, ItemStatus, SessionId};
    use crate::safety::guard::token::evidence_for;
    use crate::safety::rejection::GuardRejection;
    use std::path::PathBuf;

    fn session() -> SessionId {
        "cln_20260921103608_a1b2".parse().unwrap_or_else(|error| panic!("{error}"))
    }

    fn item(path: &str) -> CleanItem {
        CleanItem {
            path: PathBuf::from(path),
            finding_id: "user-cache.app".parse().unwrap_or_else(|error| panic!("{error}")),
            size_bytes: 4,
            status: ItemStatus::Planned,
            action: Action::Quarantine,
            error: None,
        }
    }

    fn plan(paths: &[&str]) -> CleanPlan {
        CleanPlan::dry_run(session(), paths.iter().map(|path| item(path)).collect())
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn approved(index: usize, path: &str) -> Outcome {
        Outcome { index, kind: OutcomeKind::Approved(evidence_for(path, 2, 3)) }
    }

    /// The rebuild pairs outcomes with items by position; a mismatch would attach
    /// one item's approval to another item's path.
    #[test]
    fn a_missing_outcome_is_refused_rather_than_guessed() {
        let error = rebuild(&plan(&["/Users/dana/a", "/Users/dana/b"]), &[approved(0, "/Users/dana/a")]);
        assert!(matches!(error, Err(GuardRejection::Inconsistent(_))), "{error:?}");
    }

    #[test]
    fn an_outcome_for_the_wrong_item_is_refused() {
        let outcomes = [approved(1, "/Users/dana/b"), approved(0, "/Users/dana/a")];
        let error = rebuild(&plan(&["/Users/dana/a", "/Users/dana/b"]), &outcomes);
        assert!(matches!(error, Err(GuardRejection::Inconsistent(_))), "{error:?}");
    }

    #[test]
    fn more_outcomes_than_items_is_refused() {
        let outcomes = [approved(0, "/Users/dana/a"), approved(1, "/Users/dana/b")];
        let error = rebuild(&plan(&["/Users/dana/a"]), &outcomes);
        assert!(matches!(error, Err(GuardRejection::Inconsistent(_))), "{error:?}");
    }

    #[test]
    fn a_vanished_item_is_skipped_and_worth_nothing() {
        let outcomes = [approved(0, "/Users/dana/a"), Outcome { index: 1, kind: OutcomeKind::Missing }];
        let rebuilt = rebuild(&plan(&["/Users/dana/a", "/Users/dana/b"]), &outcomes)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(rebuilt.plan.items()[1].status, ItemStatus::Skipped);
        assert_eq!(rebuilt.plan.items()[1].size_bytes, 0);
        assert_eq!(rebuilt.plan.planned_bytes(), 1, "only the surviving file counts");
        assert_eq!(rebuilt.items.len(), 1);
    }
}
