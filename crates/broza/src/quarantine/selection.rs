//! Which entries a restore is about, and where each of them is going.
//!
//! Kept apart from the restore itself because the destinations are needed
//! *before* it runs: the caller asks for them here, has them approved by
//! [`approve_restore_targets`](crate::safety::guard::approve_restore_targets),
//! and hands the token back. The list a restore writes and the list the guard
//! approved are therefore the same list, built once.

use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::model::{EntryId, QuarantineEntry, SessionId};
use crate::ports::FileOps;
use crate::quarantine::entries::sequence_in;
use crate::quarantine::putback::destination;
use crate::quarantine::store;

/// One session to restore, and which of its entries.
pub struct Wanted {
    /// The session.
    pub session: SessionId,
    /// The entries to restore; `None` means all of them.
    pub entries: Option<Vec<EntryId>>,
}

impl Wanted {
    /// Every entry of `session`.
    pub fn whole(session: &SessionId) -> Self {
        Self { session: session.clone(), entries: None }
    }

    /// `true` when `entry` is one of the entries asked for.
    pub fn covers(&self, entry: &QuarantineEntry) -> bool {
        self.entries.as_ref().is_none_or(|ids| ids.contains(&entry.id))
    }
}

/// `true` when the entry is still in the store and can therefore go back.
pub fn is_restorable(entry: &QuarantineEntry) -> bool {
    entry.stored_path.is_some()
}

/// The paths a restore of `session` will write to.
///
/// # Errors
///
/// Whatever reading the session reports.
pub fn session_destinations(
    fs: &dyn FileOps,
    root: &Path,
    session: &SessionId,
    to: Option<&Path>,
) -> Result<Vec<PathBuf>, BrozaError> {
    destinations(fs, root, &[Wanted::whole(session)], to)
}

/// The paths a restore of the named entries will write to.
///
/// # Errors
///
/// See [`session_destinations`].
pub fn entry_destinations(
    fs: &dyn FileOps,
    root: &Path,
    ids: &[EntryId],
    to: Option<&Path>,
) -> Result<Vec<PathBuf>, BrozaError> {
    destinations(fs, root, &group_by_session(ids)?, to)
}

/// Where every wanted entry is going.
///
/// # Errors
///
/// Whatever reading one of the sessions reports.
pub fn destinations(
    fs: &dyn FileOps,
    root: &Path,
    wanted: &[Wanted],
    to: Option<&Path>,
) -> Result<Vec<PathBuf>, BrozaError> {
    let mut paths = Vec::new();
    for one in wanted {
        let found = store::read_one(fs, root, &one.session)?;
        paths.extend(
            found
                .session()
                .entries
                .iter()
                .filter(|entry| is_restorable(entry) && one.covers(entry))
                .map(|entry| destination(entry, to)),
        );
    }
    Ok(paths)
}

/// The identifiers to restore, in reverse sequence order.
///
/// Reverse, because a child moved after its parent has to go back first.
pub fn restore_order(entries: Vec<QuarantineEntry>, wanted: &Wanted) -> Vec<EntryId> {
    let mut chosen: Vec<QuarantineEntry> =
        entries.into_iter().filter(|entry| is_restorable(entry) && wanted.covers(entry)).collect();
    chosen.sort_by_key(|entry| std::cmp::Reverse(sequence_in(entry).unwrap_or_default()));
    chosen.into_iter().map(|entry| entry.id).collect()
}

/// Group entry identifiers by the session they name, first appearance first.
///
/// # Errors
///
/// [`BrozaError::Usage`] when an identifier does not name a session, which a
/// validated [`EntryId`] never does.
pub fn group_by_session(ids: &[EntryId]) -> Result<Vec<Wanted>, BrozaError> {
    let mut wanted: Vec<Wanted> = Vec::new();
    for id in ids {
        let session: SessionId = id.session_part().parse()?;
        match wanted.iter_mut().find(|one| one.session == session) {
            Some(one) => one.entries.get_or_insert_with(Vec::new).push(id.clone()),
            None => wanted.push(Wanted { session, entries: Some(vec![id.clone()]) }),
        }
    }
    Ok(wanted)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Wanted, entry_destinations, group_by_session, session_destinations};
    use crate::model::ItemStatus;
    use crate::quarantine::fixtures::{ROOT, entry, quarantined, store_fs};
    use crate::quarantine::layout;

    const CACHE: &str = "/Users/dana/Library/Caches/app.cache";

    #[test]
    fn a_whole_session_goes_back_to_the_paths_it_came_from() {
        let fs = store_fs().with_sized_file(CACHE, 10);
        let session = quarantined(&fs, &[(CACHE, 10)]);

        let paths = session_destinations(&fs, Path::new(ROOT), &session.id, None)
            .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(paths, vec![PathBuf::from(CACHE)]);
    }

    #[test]
    fn a_named_entry_lands_under_its_sequence_inside_an_alternative_directory() {
        let fs = store_fs().with_sized_file(CACHE, 10);
        let session = quarantined(&fs, &[(CACHE, 10)]);
        let id = layout::entry_id(&session.id, 1).unwrap_or_else(|error| panic!("{error}"));
        let rescued = PathBuf::from("/Users/dana/Rescued");

        let paths = entry_destinations(&fs, Path::new(ROOT), &[id], Some(&rescued))
            .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(paths, vec![rescued.join("0001_app.cache")]);
    }

    #[test]
    fn an_entry_that_was_not_asked_for_has_no_destination() {
        let fs = store_fs().with_sized_file(CACHE, 10);
        let session = quarantined(&fs, &[(CACHE, 10)]);
        let other = "cln_20260801091200_c3d4/0001".parse().unwrap_or_else(|e| panic!("{e}"));
        let wanted = Wanted { session: session.id.clone(), entries: Some(vec![other]) };

        assert!(!wanted.covers(&session.entries[0]));
        assert!(Wanted::whole(&session.id).covers(&session.entries[0]));
    }

    #[test]
    fn identifiers_of_one_session_are_grouped_together() {
        let first = entry(1, CACHE, 10, ItemStatus::Quarantined).id;
        let second = entry(2, CACHE, 10, ItemStatus::Quarantined).id;

        let grouped =
            group_by_session(&[first.clone(), second.clone()]).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].entries, Some(vec![first, second]));
    }
}
