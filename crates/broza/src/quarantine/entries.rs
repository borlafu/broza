//! Building and updating the `entries` of a session manifest.
//!
//! Entries are never edited in place: every helper returns a new value, so the
//! manifest that reaches [`manifest::write`](crate::quarantine::manifest::write)
//! is always a complete, consistent document.

use crate::BrozaError;
use crate::model::{CleanPlan, ItemStatus, QuarantineEntry, QuarantineSession};
use crate::quarantine::layout;

/// The sequence number of the item at `position`, counting from one.
///
/// # Errors
///
/// [`BrozaError::Other`] when the position does not fit a sequence number; a
/// session with four billion items is a bug, not a cleanup.
pub fn sequence_of(position: usize) -> Result<u32, BrozaError> {
    u32::try_from(position).ok().and_then(|position| position.checked_add(layout::FIRST_SEQUENCE)).ok_or_else(
        || BrozaError::Other("quarantine session: more items than a sequence number can name".to_owned()),
    )
}

/// The sequence number an entry's identifier carries.
///
/// `None` when the identifier does not end in a number a `u32` can hold, which
/// a validated [`EntryId`](crate::model::EntryId) never does — a manifest that
/// produced one is corrupt, and the caller decides what that means rather than
/// silently getting `0` and restoring in the wrong order.
pub fn sequence_in(entry: &QuarantineEntry) -> Option<u32> {
    entry.id.sequence_part().parse().ok()
}

/// One entry per plan item named by `indices`, before anything has been moved.
///
/// # Errors
///
/// [`BrozaError::Other`] when an index does not name an item of the plan, and
/// [`BrozaError::Usage`] when the pair does not form a valid entry identifier.
pub fn planned_entries(plan: &CleanPlan, indices: &[usize]) -> Result<Vec<QuarantineEntry>, BrozaError> {
    indices
        .iter()
        .enumerate()
        .map(|(position, index)| {
            let item = plan.items().get(*index).ok_or_else(|| {
                BrozaError::Other(format!("quarantine session: the plan has no item {index}"))
            })?;
            Ok(QuarantineEntry {
                id: layout::entry_id(plan.session_id(), sequence_of(position)?)?,
                original_path: item.path.clone(),
                stored_path: None,
                restored_to: None,
                size_bytes: item.size_bytes,
                status: ItemStatus::Planned,
                error: None,
            })
        })
        .collect()
}

/// The session with the entry at `position` replaced and its totals recomputed.
pub fn with_entry(
    session: &QuarantineSession,
    position: usize,
    entry: &QuarantineEntry,
) -> QuarantineSession {
    let entries: Vec<QuarantineEntry> = session
        .entries
        .iter()
        .enumerate()
        .map(|(index, existing)| if index == position { entry.clone() } else { existing.clone() })
        .collect();
    with_entries(session, entries)
}

/// The session carrying `entries`, with `total_bytes` and `item_count` derived.
pub fn with_entries(session: &QuarantineSession, entries: Vec<QuarantineEntry>) -> QuarantineSession {
    QuarantineSession {
        total_bytes: held_bytes(&entries),
        item_count: u64::try_from(entries.len()).unwrap_or(u64::MAX),
        entries,
        ..session.clone()
    }
}

/// Bytes the store actually holds: the entries that still have a stored path.
///
/// `stored_path` rather than `status` is the question, because the two can
/// disagree in the one case that matters: an entry a restore could not put back
/// is reported as `skipped` and is still sitting in the store.
pub fn held_bytes(entries: &[QuarantineEntry]) -> u64 {
    entries
        .iter()
        .filter(|entry| entry.stored_path.is_some())
        .fold(0_u64, |sum, entry| sum.saturating_add(entry.size_bytes))
}

/// `true` when nothing of the session is left inside the store.
pub fn is_emptied(entries: &[QuarantineEntry]) -> bool {
    entries.iter().all(|entry| entry.stored_path.is_none())
}

#[cfg(test)]
mod tests {
    use super::{
        held_bytes, is_emptied, planned_entries, sequence_in, sequence_of, with_entries, with_entry,
    };
    use crate::model::{Action, CleanItem, CleanPlan, ItemStatus, QuarantineEntry, SessionState};
    use crate::quarantine::fixtures::{entry, session, session_id};

    fn plan(paths: &[(&str, u64)]) -> CleanPlan {
        let items = paths
            .iter()
            .map(|(path, size_bytes)| CleanItem {
                path: (*path).into(),
                finding_id: "user-cache.app".parse().unwrap_or_else(|error| panic!("{error}")),
                size_bytes: *size_bytes,
                status: ItemStatus::Planned,
                action: Action::Quarantine,
                error: None,
                snapshot: None,
            })
            .collect();
        CleanPlan::dry_run(session_id(), items).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn sequence_numbers_start_at_one() {
        assert_eq!(sequence_of(0).ok(), Some(1));
        assert_eq!(sequence_of(41).ok(), Some(42));
        assert!(sequence_of(usize::MAX).is_err());
    }

    #[test]
    fn an_entry_reports_the_sequence_of_its_identifier() {
        assert_eq!(sequence_in(&entry(7, "/Users/dana/a", 1, ItemStatus::Quarantined)), Some(7));
    }

    #[test]
    fn planned_entries_take_the_path_and_size_of_the_plan() {
        let plan = plan(&[("/Users/dana/a", 10), ("/Users/dana/b", 20)]);

        let entries = planned_entries(&plan, &[1]).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].original_path, std::path::PathBuf::from("/Users/dana/b"));
        assert_eq!(entries[0].size_bytes, 20);
        assert_eq!(entries[0].status, ItemStatus::Planned);
        assert!(entries[0].stored_path.is_none());
        assert_eq!(entries[0].id.sequence_part(), "0001", "the sequence counts entries, not plan items");
    }

    #[test]
    fn an_index_outside_the_plan_is_an_error_not_a_panic() {
        assert!(planned_entries(&plan(&[("/Users/dana/a", 10)]), &[3]).is_err());
    }

    #[test]
    fn only_the_entries_still_in_the_store_hold_bytes() {
        let entries = vec![
            entry(1, "/Users/dana/a", 10, ItemStatus::Quarantined),
            entry(2, "/Users/dana/b", 20, ItemStatus::Skipped),
            entry(3, "/Users/dana/c", 40, ItemStatus::Restored),
        ];

        assert_eq!(held_bytes(&entries), 10);
        assert!(!is_emptied(&entries));
        assert!(is_emptied(&entries[1..]), "neither of those is in the store any more");
    }

    #[test]
    fn an_entry_a_restore_could_not_put_back_is_still_held() {
        let stuck = QuarantineEntry {
            status: ItemStatus::Skipped,
            ..entry(1, "/Users/dana/a", 10, ItemStatus::Quarantined)
        };

        assert_eq!(held_bytes(std::slice::from_ref(&stuck)), 10);
        assert!(!is_emptied(std::slice::from_ref(&stuck)));
    }

    #[test]
    fn replacing_an_entry_recomputes_the_totals() {
        let before = session(
            SessionState::InProgress,
            vec![
                entry(1, "/Users/dana/a", 10, ItemStatus::Planned),
                entry(2, "/Users/dana/b", 20, ItemStatus::Planned),
            ],
        );
        let moved = entry(2, "/Users/dana/b", 20, ItemStatus::Quarantined);

        let after = with_entry(&before, 1, &moved);

        assert_eq!(after.total_bytes, 20);
        assert_eq!(after.item_count, 2);
        assert_eq!(after.entries[0].status, ItemStatus::Planned);
        assert_eq!(after.entries[1].status, ItemStatus::Quarantined);
    }

    #[test]
    fn dropping_every_entry_empties_the_totals() {
        let before =
            session(SessionState::Complete, vec![entry(1, "/Users/dana/a", 10, ItemStatus::Quarantined)]);

        let after = with_entries(&before, Vec::new());

        assert_eq!((after.total_bytes, after.item_count), (0, 0));
    }
}
