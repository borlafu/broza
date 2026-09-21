//! `clean` payload: the plan and its items (`docs/cli-spec.md` §4.4).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::BrozaError;
use crate::model::ids::SessionId;
use crate::model::plan_item::{CleanItem, ExpiredSession};
use crate::model::status::{ItemErrorCode, ItemStatus};

/// Payload of `broza clean`, in dry-run and in `--apply` mode alike.
///
/// The byte counters are normative (`docs/cli-spec.md` §4.4): `quarantined_bytes` are
/// still on disk until expiry or purge and are never added to `reclaimed_bytes`
/// (`AGENTS.md` §2.7).
///
/// The fields are private. A plan is built with [`CleanPlan::new`], parsed through
/// [`CleanPlanRepr`] or derived from another plan with the `with_*` helpers; all of
/// them end in [`CleanPlan::validate`], so a plan that contradicts §4.4 cannot exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", try_from = "CleanPlanRepr")]
pub struct CleanPlan {
    /// `true` when nothing was written to disk.
    dry_run: bool,
    /// Identifier of the cleanup session.
    session_id: SessionId,
    /// Sum of `size_bytes` of every item in the plan, whatever its outcome.
    planned_bytes: u64,
    /// Bytes moved into quarantine in this run. Pending, not yet reclaimed.
    quarantined_bytes: u64,
    /// Bytes actually freed in this run: purges, `tmutil` deletions and expired sessions.
    reclaimed_bytes: u64,
    /// Directory holding the quarantined items. Absent in a dry run.
    #[serde(skip_serializing_if = "Option::is_none")]
    quarantine_path: Option<PathBuf>,
    /// Sessions expired during the pre-execution step; their bytes count as reclaimed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    expired_sessions: Vec<ExpiredSession>,
    /// The items of the plan.
    items: Vec<CleanItem>,
}

/// Wire shape of a [`CleanPlan`]: the only way into one, from JSON or from code.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CleanPlanRepr {
    /// See [`CleanPlan::is_dry_run`].
    pub dry_run: bool,
    /// See [`CleanPlan::session_id`].
    pub session_id: SessionId,
    /// See [`CleanPlan::planned_bytes`].
    #[serde(default)]
    pub planned_bytes: u64,
    /// See [`CleanPlan::quarantined_bytes`].
    #[serde(default)]
    pub quarantined_bytes: u64,
    /// See [`CleanPlan::reclaimed_bytes`].
    #[serde(default)]
    pub reclaimed_bytes: u64,
    /// See [`CleanPlan::quarantine_path`].
    #[serde(default)]
    pub quarantine_path: Option<PathBuf>,
    /// See [`CleanPlan::expired_sessions`].
    #[serde(default)]
    pub expired_sessions: Vec<ExpiredSession>,
    /// See [`CleanPlan::items`].
    #[serde(default)]
    pub items: Vec<CleanItem>,
}

impl TryFrom<CleanPlanRepr> for CleanPlan {
    type Error = BrozaError;

    fn try_from(repr: CleanPlanRepr) -> Result<Self, Self::Error> {
        let plan = Self {
            dry_run: repr.dry_run,
            session_id: repr.session_id,
            planned_bytes: repr.planned_bytes,
            quarantined_bytes: repr.quarantined_bytes,
            reclaimed_bytes: repr.reclaimed_bytes,
            quarantine_path: repr.quarantine_path,
            expired_sessions: repr.expired_sessions,
            items: repr.items,
        };
        plan.validate()?;
        Ok(plan)
    }
}

impl CleanPlan {
    /// Build a dry-run plan: `planned_bytes` is derived, nothing is moved yet.
    pub fn dry_run(session_id: SessionId, items: Vec<CleanItem>) -> Result<Self, BrozaError> {
        let planned_bytes = sum_sizes(&items)
            .ok_or_else(|| BrozaError::Other("clean plan: the item sizes overflow u64".to_owned()))?;
        Self::try_from(CleanPlanRepr {
            dry_run: true,
            session_id,
            planned_bytes,
            quarantined_bytes: 0,
            reclaimed_bytes: 0,
            quarantine_path: None,
            expired_sessions: Vec::new(),
            items,
        })
    }

    /// Build any plan from its parts, validating the result.
    pub fn new(repr: CleanPlanRepr) -> Result<Self, BrozaError> {
        Self::try_from(repr)
    }

    /// `true` when nothing was written to disk.
    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// Identifier of the cleanup session.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Sum of the sizes of every item in the plan.
    pub fn planned_bytes(&self) -> u64 {
        self.planned_bytes
    }

    /// Bytes moved into quarantine in this run.
    pub fn quarantined_bytes(&self) -> u64 {
        self.quarantined_bytes
    }

    /// Bytes actually freed in this run.
    pub fn reclaimed_bytes(&self) -> u64 {
        self.reclaimed_bytes
    }

    /// Directory holding the quarantined items.
    pub fn quarantine_path(&self) -> Option<&PathBuf> {
        self.quarantine_path.as_ref()
    }

    /// Sessions expired during the pre-execution step.
    pub fn expired_sessions(&self) -> &[ExpiredSession] {
        &self.expired_sessions
    }

    /// The items of the plan.
    pub fn items(&self) -> &[CleanItem] {
        &self.items
    }

    /// Return a copy of the plan in `--apply` mode, rooted at `quarantine_path`.
    pub fn into_applied(self, quarantine_path: Option<PathBuf>) -> Result<Self, BrozaError> {
        Self::try_from(CleanPlanRepr { dry_run: false, quarantine_path, ..self.into_repr() })
    }

    /// Return a copy with the outcome of item `index` replaced.
    ///
    /// The executor records progress this way: nothing is mutated in place, and the
    /// result is validated, so a `purged` item can never keep an error code.
    pub fn with_item_status(
        self,
        index: usize,
        status: ItemStatus,
        error: Option<ItemErrorCode>,
    ) -> Result<Self, BrozaError> {
        let mut repr = self.into_repr();
        let item = repr
            .items
            .get(index)
            .ok_or_else(|| BrozaError::Other(format!("clean plan: no item at index {index}")))?;
        let updated = CleanItem { status, error, ..item.clone() };
        repr.items = replace_at(repr.items, index, &updated);
        Self::try_from(repr)
    }

    /// Return a copy with the byte counters replaced.
    pub fn with_bytes(self, quarantined_bytes: u64, reclaimed_bytes: u64) -> Result<Self, BrozaError> {
        Self::try_from(CleanPlanRepr { quarantined_bytes, reclaimed_bytes, ..self.into_repr() })
    }

    /// Return a copy with the expired sessions replaced.
    pub fn with_expired_sessions(self, expired_sessions: Vec<ExpiredSession>) -> Result<Self, BrozaError> {
        Self::try_from(CleanPlanRepr { expired_sessions, ..self.into_repr() })
    }

    /// Check the invariants of `docs/cli-spec.md` §4.4.
    ///
    /// 1. `planned_bytes` is the sum of the item sizes.
    /// 2. An item carries an `error` only when it was skipped or failed.
    /// 3. A dry run quarantines nothing, reclaims nothing, has no quarantine
    ///    directory, no expired sessions and only `planned` items.
    pub fn validate(&self) -> Result<(), BrozaError> {
        let inconsistent =
            |reason: &str| Err(BrozaError::Other(format!("clean plan `{}`: {reason}", self.session_id)));
        if sum_sizes(&self.items) != Some(self.planned_bytes) {
            return inconsistent("`planned_bytes` is not the sum of the item sizes");
        }
        if self.items.iter().any(|item| item.error.is_some() && !item.status.is_unsuccessful()) {
            return inconsistent("only skipped or failed items carry an `error`");
        }
        if !self.dry_run {
            return Ok(());
        }
        if self.quarantined_bytes != 0
            || self.reclaimed_bytes != 0
            || self.quarantine_path.is_some()
            || !self.expired_sessions.is_empty()
        {
            return inconsistent("a dry run neither quarantines nor reclaims bytes");
        }
        if self.items.iter().any(|item| item.status != ItemStatus::Planned) {
            return inconsistent("every item of a dry run is `planned`");
        }
        Ok(())
    }

    /// Decompose the plan into its wire shape.
    fn into_repr(self) -> CleanPlanRepr {
        CleanPlanRepr {
            dry_run: self.dry_run,
            session_id: self.session_id,
            planned_bytes: self.planned_bytes,
            quarantined_bytes: self.quarantined_bytes,
            reclaimed_bytes: self.reclaimed_bytes,
            quarantine_path: self.quarantine_path,
            expired_sessions: self.expired_sessions,
            items: self.items,
        }
    }
}

/// Sum the item sizes, `None` on overflow.
fn sum_sizes(items: &[CleanItem]) -> Option<u64> {
    items.iter().try_fold(0_u64, |sum, item| sum.checked_add(item.size_bytes))
}

/// Return `items` with the entry at `index` replaced by `item`.
fn replace_at(items: Vec<CleanItem>, index: usize, item: &CleanItem) -> Vec<CleanItem> {
    items
        .into_iter()
        .enumerate()
        .map(|(position, existing)| if position == index { item.clone() } else { existing })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{CleanItem, CleanPlan, CleanPlanRepr, ExpiredSession, ItemErrorCode, ItemStatus};
    use crate::model::finding::Action;
    use crate::model::ids::SessionId;

    fn session() -> SessionId {
        "cln_20260921103608_a1b2".parse().unwrap_or_else(|e| panic!("{e}"))
    }

    fn item(size_bytes: u64, status: ItemStatus) -> CleanItem {
        CleanItem {
            path: "/Users/x/Library/Caches/example".into(),
            finding_id: "user-cache.logs".parse().unwrap_or_else(|e| panic!("{e}")),
            size_bytes,
            status,
            action: Action::Quarantine,
            error: None,
        }
    }

    fn dry_run(items: Vec<CleanItem>) -> CleanPlan {
        CleanPlan::dry_run(session(), items).unwrap_or_else(|e| panic!("{e}"))
    }

    /// A valid dry-run shape, for tests that then break one member of it.
    fn repr(items: Vec<CleanItem>) -> CleanPlanRepr {
        CleanPlanRepr {
            dry_run: true,
            session_id: session(),
            planned_bytes: items.iter().map(|item| item.size_bytes).sum(),
            quarantined_bytes: 0,
            reclaimed_bytes: 0,
            quarantine_path: None,
            expired_sessions: Vec::new(),
            items,
        }
    }

    #[test]
    fn a_dry_run_plan_derives_its_total_and_moves_nothing() {
        let plan = dry_run(vec![item(10, ItemStatus::Planned), item(32, ItemStatus::Planned)]);
        assert_eq!(plan.planned_bytes(), 42);
        assert_eq!(plan.quarantined_bytes(), 0);
        assert_eq!(plan.reclaimed_bytes(), 0);
        assert!(plan.is_dry_run());
        assert!(plan.quarantine_path().is_none());
        assert!(plan.expired_sessions().is_empty());
        assert_eq!(plan.items().len(), 2);
        assert_eq!(plan.session_id(), &session());
        assert!(plan.validate().is_ok());
    }

    #[test]
    fn a_dry_run_that_reports_movement_cannot_be_built() {
        let base = || repr(vec![item(10, ItemStatus::Planned)]);
        assert!(CleanPlan::new(CleanPlanRepr { quarantined_bytes: 10, ..base() }).is_err());
        assert!(CleanPlan::new(CleanPlanRepr { reclaimed_bytes: 10, ..base() }).is_err());
        assert!(CleanPlan::new(CleanPlanRepr { quarantine_path: Some("/tmp/q".into()), ..base() }).is_err());
        assert!(
            CleanPlan::new(CleanPlanRepr { items: vec![item(10, ItemStatus::Quarantined)], ..base() })
                .is_err()
        );
        assert!(CleanPlan::new(base()).is_ok());
    }

    #[test]
    fn planned_bytes_must_be_the_sum_of_the_item_sizes() {
        let wrong_total = CleanPlanRepr { planned_bytes: 99, ..repr(vec![item(10, ItemStatus::Planned)]) };
        assert!(CleanPlan::new(wrong_total).is_err());
        assert!(
            CleanPlan::dry_run(
                session(),
                vec![item(u64::MAX, ItemStatus::Planned), item(1, ItemStatus::Planned)]
            )
            .is_err()
        );
    }

    #[test]
    fn an_applied_plan_records_progress_immutably() {
        let plan = dry_run(vec![item(10, ItemStatus::Planned), item(32, ItemStatus::Planned)]);
        let applied = plan
            .clone()
            .into_applied(Some("/Users/x/.local/share/broza/quarantine".into()))
            .unwrap_or_else(|e| panic!("{e}"));
        let executed = applied
            .with_item_status(0, ItemStatus::Quarantined, None)
            .and_then(|plan| plan.with_item_status(1, ItemStatus::Skipped, Some(ItemErrorCode::CrossVolume)))
            .and_then(|plan| plan.with_bytes(10, 0))
            .unwrap_or_else(|e| panic!("{e}"));

        assert!(plan.is_dry_run(), "the original plan was mutated");
        assert_eq!(plan.items()[0].status, ItemStatus::Planned);
        assert_eq!(executed.items()[0].status, ItemStatus::Quarantined);
        assert_eq!(executed.items()[1].error, Some(ItemErrorCode::CrossVolume));
        assert_eq!(executed.quarantined_bytes(), 10);
        assert!(executed.with_item_status(9, ItemStatus::Purged, None).is_err());
    }

    #[test]
    fn expired_sessions_belong_to_an_applied_plan_only() {
        let expired = vec![ExpiredSession {
            id: "cln_20260801091200_c3d4".parse().unwrap_or_else(|e| panic!("{e}")),
            freed_bytes: 12_400_000_000,
        }];
        let plan = dry_run(vec![item(10, ItemStatus::Planned)]);
        assert!(plan.clone().with_expired_sessions(expired.clone()).is_err());

        let applied = plan
            .into_applied(None)
            .and_then(|plan| plan.with_expired_sessions(expired))
            .and_then(|plan| plan.with_bytes(0, 12_400_000_000))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(applied.expired_sessions().len(), 1);
        assert_eq!(applied.reclaimed_bytes(), 12_400_000_000);
    }

    #[test]
    fn only_skipped_or_failed_items_may_carry_an_error() {
        let applied = |item: CleanItem| CleanPlan::new(CleanPlanRepr { dry_run: false, ..repr(vec![item]) });
        for status in [ItemStatus::Skipped, ItemStatus::Failed] {
            assert!(status.is_unsuccessful(), "{status}");
            let failed = CleanItem { error: Some(ItemErrorCode::CrossVolume), ..item(10, status.clone()) };
            assert!(applied(failed).is_ok(), "{status}");
        }
        for status in [ItemStatus::Planned, ItemStatus::Quarantined, ItemStatus::Purged, ItemStatus::Restored]
        {
            assert!(!status.is_unsuccessful(), "{status}");
            let bogus = CleanItem { error: Some(ItemErrorCode::Collision), ..item(10, status.clone()) };
            assert!(applied(bogus).is_err(), "{status}");
        }
    }
}
