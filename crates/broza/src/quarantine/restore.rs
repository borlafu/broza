//! `broza restore` (`docs/cli-spec.md` §3.5 and §4.6).
//!
//! One session at a time, entries in reverse sequence order so a child that was
//! moved after its parent goes back first. The manifest is rewritten after every
//! entry, so an interrupted restore leaves a session in state `restoring` that
//! can simply be run again. A session whose entries all went back is deleted.

use std::path::Path;

use crate::BrozaError;
use crate::model::{
    EntryId, ErrorEntry, ItemStatus, OperationKind, QuarantineEntry, QuarantineSession, RestoreReport,
    RestoreSession, SessionId, SessionState, Warning,
};
use crate::ports::FileOps;
use crate::quarantine::entries::{is_emptied, sequence_in, with_entries};
use crate::quarantine::guarded::{Recheck, recheck};
use crate::quarantine::layout;
use crate::quarantine::manifest::{self, Manifest};
use crate::quarantine::putback::put_back;
use crate::quarantine::report::{Reported, diagnostic};
use crate::quarantine::store::{self, StoredSession};
use crate::safety::guard::{Approved, QuarantineWrite};

/// Warning code for a session that was emptied but could not be removed.
pub const SESSION_LEFT_BEHIND: &str = "session_left_behind";

/// Restore every item of one session.
///
/// # Errors
///
/// [`BrozaError::TargetNotFound`] for a session the store does not hold (exit
/// `4`), [`BrozaError::Other`] when the token does not cover a stored path, and
/// whatever writing the manifest reports.
pub fn restore_session(
    token: &Approved<QuarantineWrite>,
    session: &SessionId,
    root: &Path,
    fs: &dyn FileOps,
    to: Option<&Path>,
) -> Result<Reported<RestoreReport>, BrozaError> {
    restore_all(token, &[Wanted { session: session.clone(), entries: None }], root, fs, to)
}

/// Restore the named items, which may belong to several sessions.
///
/// # Errors
///
/// See [`restore_session`], plus [`BrozaError::Usage`] when an identifier does
/// not name a session the store holds.
pub fn restore_entries(
    token: &Approved<QuarantineWrite>,
    ids: &[EntryId],
    root: &Path,
    fs: &dyn FileOps,
    to: Option<&Path>,
) -> Result<Reported<RestoreReport>, BrozaError> {
    restore_all(token, &group_by_session(ids)?, root, fs, to)
}

/// One session to restore, and which of its entries.
struct Wanted {
    /// The session.
    session: SessionId,
    /// The entries to restore; `None` means all of them.
    entries: Option<Vec<EntryId>>,
}

impl Wanted {
    /// `true` when `entry` is one of the entries asked for.
    fn covers(&self, entry: &QuarantineEntry) -> bool {
        self.entries.as_ref().is_none_or(|ids| ids.contains(&entry.id))
    }
}

/// What restoring one session produced.
#[allow(clippy::struct_field_names, reason = "`restored_bytes` is the name the JSON contract uses")]
struct Restored {
    /// The session line of the report.
    session: RestoreSession,
    /// One `errors[]` entry per item that did not go back.
    errors: Vec<ErrorEntry>,
    /// One `warnings[]` entry when the emptied session could not be removed.
    warnings: Vec<Warning>,
    /// Bytes moved back.
    restored_bytes: u64,
}

/// Restore each wanted session in turn and merge the reports.
fn restore_all(
    token: &Approved<QuarantineWrite>,
    wanted: &[Wanted],
    root: &Path,
    fs: &dyn FileOps,
    to: Option<&Path>,
) -> Result<Reported<RestoreReport>, BrozaError> {
    let done = wanted
        .iter()
        .map(|one| restore_one(token, one, root, fs, to))
        .collect::<Result<Vec<Restored>, BrozaError>>()?;
    let report = RestoreReport {
        operation: OperationKind::Restore,
        restored_bytes: done.iter().fold(0_u64, |sum, one| sum.saturating_add(one.restored_bytes)),
        sessions: done.iter().map(|one| one.session.clone()).collect(),
    };
    let errors = done.iter().flat_map(|one| one.errors.clone()).collect();
    let warnings = done.into_iter().flat_map(|one| one.warnings).collect();
    Ok(Reported::with(report, errors, warnings))
}

/// Mark the session `restoring`, put its items back, then close it.
fn restore_one(
    token: &Approved<QuarantineWrite>,
    wanted: &Wanted,
    root: &Path,
    fs: &dyn FileOps,
    to: Option<&Path>,
) -> Result<Restored, BrozaError> {
    let found = store::read_one(fs, root, &wanted.session)?;
    let manifest = write_state(&found.dir, &found.manifest, SessionState::Restoring, fs)?;
    let order = restore_order(manifest.session.entries.clone(), wanted);
    let pass = order.iter().try_fold(Pass { manifest, reported: Vec::new() }, |pass, id| {
        put_one(pass, id, &found, token, fs, to)
    })?;
    let touched = in_sequence_order(pass.reported);
    let warnings = close(&found, &pass.manifest, token, fs)?;
    Ok(Restored {
        session: RestoreSession {
            id: found.id.clone(),
            status: session_status(&touched),
            items: touched.clone(),
        },
        errors: touched.iter().filter_map(|entry| failure_of(entry, &found)).collect(),
        warnings,
        restored_bytes: restored_bytes(&touched),
    })
}

/// The identifiers to restore, in reverse sequence order.
///
/// Only an entry that is actually in the store can go back: one already
/// restored, or skipped when it was quarantined, has nothing to move.
fn restore_order(entries: Vec<QuarantineEntry>, wanted: &Wanted) -> Vec<EntryId> {
    let mut chosen: Vec<QuarantineEntry> = entries
        .into_iter()
        .filter(|entry| entry.status == ItemStatus::Quarantined && wanted.covers(entry))
        .collect();
    chosen.sort_by_key(|entry| std::cmp::Reverse(sequence_in(entry)));
    chosen.into_iter().map(|entry| entry.id).collect()
}

/// The manifest as it stands, and what each attempt is reported as.
struct Pass {
    /// The manifest as it was last written.
    manifest: Manifest,
    /// One entry per attempt, in the order they were attempted.
    reported: Vec<QuarantineEntry>,
}

/// Put one entry back and persist the manifest that describes the result.
///
/// The manifest records what the store *holds*, so it is only rewritten when
/// the item actually left: an entry that was skipped or failed stays
/// `quarantined` on disk and can be restored again later
/// (`docs/cli-spec.md` §3.5, "can be retried"). The report says what happened.
fn put_one(
    pass: Pass,
    id: &EntryId,
    found: &StoredSession,
    token: &Approved<QuarantineWrite>,
    fs: &dyn FileOps,
    to: Option<&Path>,
) -> Result<Pass, BrozaError> {
    let Pass { manifest, reported } = pass;
    let Some(position) = manifest.session.entries.iter().position(|entry| entry.id == *id) else {
        return Ok(Pass { manifest, reported });
    };
    let entry = manifest.session.entries.get(position).ok_or_else(|| missing(id))?;
    let put = put_back(entry, to, token.items(), fs)?;
    let reported = [reported, vec![put.clone()]].concat();
    if put.status != ItemStatus::Restored {
        return Ok(Pass { manifest, reported });
    }
    let entries = manifest
        .session
        .entries
        .iter()
        .enumerate()
        .map(|(index, existing)| if index == position { put.clone() } else { existing.clone() })
        .collect();
    let session = with_entries(&manifest.session, entries);
    let updated = manifest.with_session(session);
    manifest::write(fs, &layout::manifest_path(&found.dir), &updated)?;
    Ok(Pass { manifest: updated, reported })
}

/// Remove an emptied session, or record what it still holds.
///
/// The session directory is only removed once every entry left it. Removing it
/// is itself a write inside the store, so the token has to cover it as well; a
/// caller that approved only the stored items gets a warning and an empty
/// session left behind rather than a failed restore.
fn close(
    found: &StoredSession,
    manifest: &Manifest,
    token: &Approved<QuarantineWrite>,
    fs: &dyn FileOps,
) -> Result<Vec<Warning>, BrozaError> {
    if !is_emptied(&manifest.session.entries) {
        write_state(&found.dir, manifest, SessionState::Complete, fs)?;
        return Ok(Vec::new());
    }
    match drop_session(found, token, fs) {
        Ok(()) => Ok(Vec::new()),
        Err(error) => {
            write_state(&found.dir, manifest, SessionState::Complete, fs)?;
            Ok(vec![diagnostic(
                SESSION_LEFT_BEHIND,
                format!("quarantine session `{}` is empty but was not removed: {error}", found.id),
                Some(&found.dir),
            )])
        }
    }
}

/// Re-check the session directory against the token and remove it.
fn drop_session(
    found: &StoredSession,
    token: &Approved<QuarantineWrite>,
    fs: &dyn FileOps,
) -> Result<(), BrozaError> {
    match recheck(token.items(), &found.dir, fs)? {
        Recheck::Approved => fs.remove_tree(&found.dir),
        Recheck::Refused(code) => {
            Err(BrozaError::Other(format!("the directory changed since it was checked ({code})")))
        }
    }
}

/// Rewrite the manifest of `dir` with a new state, keeping everything else.
fn write_state(
    dir: &Path,
    manifest: &Manifest,
    state: SessionState,
    fs: &dyn FileOps,
) -> Result<Manifest, BrozaError> {
    let session = QuarantineSession { state, ..manifest.session.clone() };
    let updated = manifest.clone().with_session(session);
    manifest::write(fs, &layout::manifest_path(dir), &updated)?;
    Ok(updated)
}

/// The entries the report shows, in sequence order rather than restore order.
fn in_sequence_order(mut items: Vec<QuarantineEntry>) -> Vec<QuarantineEntry> {
    items.sort_by_key(sequence_in);
    items
}

/// The outcome of the session as a whole.
fn session_status(items: &[QuarantineEntry]) -> ItemStatus {
    if items.iter().any(|entry| entry.status == ItemStatus::Failed) {
        return ItemStatus::Failed;
    }
    if items.iter().any(|entry| entry.status == ItemStatus::Skipped) {
        return ItemStatus::Skipped;
    }
    ItemStatus::Restored
}

/// Bytes the restore actually moved back.
fn restored_bytes(items: &[QuarantineEntry]) -> u64 {
    items
        .iter()
        .filter(|entry| entry.status == ItemStatus::Restored)
        .fold(0_u64, |sum, entry| sum.saturating_add(entry.size_bytes))
}

/// The `errors[]` entry an item that did not go back earns.
fn failure_of(entry: &QuarantineEntry, found: &StoredSession) -> Option<ErrorEntry> {
    let code = entry.error.clone().filter(|_| entry.status.is_unsuccessful())?;
    Some(diagnostic(
        code.as_str(),
        format!("`{}` of session `{}` was not restored: {code}", entry.id, found.id),
        Some(&entry.original_path),
    ))
}

/// Group entry identifiers by the session they name, first appearance first.
fn group_by_session(ids: &[EntryId]) -> Result<Vec<Wanted>, BrozaError> {
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

/// An entry that disappeared from the manifest between two reads.
fn missing(id: &EntryId) -> BrozaError {
    BrozaError::Other(format!("quarantine restore: entry `{id}` is no longer in the manifest"))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        SESSION_LEFT_BEHIND, Wanted, group_by_session, restore_entries, restore_order, restore_session,
    };
    use crate::BrozaError;
    use crate::model::{
        EntryId, ItemErrorCode, ItemStatus, OperationKind, QuarantineSession, RestoreReport, SessionId,
        SessionState,
    };
    use crate::ports::FileOps;
    use crate::quarantine::fixtures::{
        ROOT, entry, quarantine_write, quarantined, session_dir, session_id, store_fs, writable_paths,
    };
    use crate::quarantine::report::Reported;
    use crate::quarantine::store;
    use crate::testing::FakeFileOps;

    const CACHE: &str = "/Users/dana/Library/Caches/app.cache";
    const OTHER: &str = "/Users/dana/Library/Caches/other.cache";

    fn two_items() -> (FakeFileOps, QuarantineSession) {
        let fs = store_fs().with_sized_file(CACHE, 10).with_sized_file(OTHER, 20);
        let session = quarantined(&fs, &[(CACHE, 10), (OTHER, 20)]);
        (fs, session)
    }

    fn restored(fs: &FakeFileOps, session: &QuarantineSession, to: Option<&Path>) -> Reported<RestoreReport> {
        let token = quarantine_write(fs, &writable_paths(session));
        restore_session(&token, &session_id(), Path::new(ROOT), fs, to)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn entry_id(sequence: u32) -> EntryId {
        crate::quarantine::layout::entry_id(&session_id(), sequence).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn a_restored_session_puts_every_item_back_and_disappears() {
        let (fs, session) = two_items();

        let reported = restored(&fs, &session, None);

        assert_eq!(reported.data.operation, OperationKind::Restore);
        assert_eq!(reported.data.restored_bytes, 30);
        assert_eq!(reported.data.sessions[0].status, ItemStatus::Restored);
        assert!(fs.exists(Path::new(CACHE)) && fs.exists(Path::new(OTHER)));
        assert!(!fs.exists(&session_dir()), "an emptied session is deleted");
        assert!(!reported.is_partial());
    }

    #[test]
    fn a_restored_item_reports_where_it_went_and_no_longer_has_a_stored_path() {
        let (fs, session) = two_items();

        let reported = restored(&fs, &session, None);

        let items = &reported.data.sessions[0].items;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].restored_to, Some(PathBuf::from(CACHE)));
        assert!(items[0].stored_path.is_none());
        assert_eq!(items[0].id.sequence_part(), "0001", "the report is in sequence order");
    }

    #[test]
    fn an_occupied_original_path_is_skipped_and_its_session_survives() {
        let (fs, session) = two_items();
        fs.add_file(CACHE, b"something new");

        let reported = restored(&fs, &session, None);

        assert_eq!(reported.data.sessions[0].status, ItemStatus::Skipped);
        assert_eq!(reported.data.restored_bytes, 20, "the other item still went back");
        assert_eq!(reported.errors.len(), 1);
        assert_eq!(reported.errors[0].code, ItemErrorCode::Collision.to_string());
        assert!(reported.is_partial(), "a skipped entry means exit 5");
        let left =
            store::read_one(&fs, Path::new(ROOT), &session_id()).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(left.session().state, SessionState::Complete);
        assert_eq!(left.session().total_bytes, 10, "only the skipped item is still held");
    }

    #[test]
    fn restoring_to_another_directory_never_touches_the_original_paths() {
        let (fs, session) = two_items();
        let elsewhere = PathBuf::from("/Users/dana/Rescued");

        let reported = restored(&fs, &session, Some(&elsewhere));

        assert_eq!(reported.data.restored_bytes, 30);
        assert!(fs.exists(&elsewhere.join("0001_app.cache")));
        assert!(fs.exists(&elsewhere.join("0002_other.cache")));
        assert!(!fs.exists(Path::new(CACHE)));
    }

    #[test]
    fn restoring_one_entry_leaves_the_rest_of_the_session_alone() {
        let (fs, session) = two_items();
        let token = quarantine_write(&fs, &writable_paths(&session));

        let reported = restore_entries(&token, &[entry_id(1)], Path::new(ROOT), &fs, None)
            .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(reported.data.sessions[0].items.len(), 1);
        assert_eq!(reported.data.restored_bytes, 10);
        assert!(fs.exists(Path::new(CACHE)) && !fs.exists(Path::new(OTHER)));
        let left =
            store::read_one(&fs, Path::new(ROOT), &session_id()).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(left.session().state, SessionState::Complete);
        assert_eq!(left.session().entries[0].status, ItemStatus::Restored);
        assert_eq!(left.session().entries[1].status, ItemStatus::Quarantined);
    }

    #[test]
    fn a_session_whose_directory_was_not_approved_is_emptied_and_reported() {
        let (fs, session) = two_items();
        let stored_only: Vec<PathBuf> =
            session.entries.iter().filter_map(|entry| entry.stored_path.clone()).collect();
        let token = quarantine_write(&fs, &stored_only);

        let reported = restore_session(&token, &session_id(), Path::new(ROOT), &fs, None)
            .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(reported.data.sessions[0].status, ItemStatus::Restored);
        assert_eq!(reported.warnings[0].code, SESSION_LEFT_BEHIND);
        assert!(fs.exists(&session_dir()), "an unapproved directory is never removed");
        assert!(!reported.is_partial(), "the items did go back; only the empty shell stayed");
    }

    #[test]
    fn an_unknown_session_is_a_missing_target() {
        let (fs, session) = two_items();
        let token = quarantine_write(&fs, &writable_paths(&session));
        let ghost: SessionId = "cln_20260801091200_c3d4".parse().unwrap_or_else(|e| panic!("{e}"));

        let error = restore_session(&token, &ghost, Path::new(ROOT), &fs, None);

        assert!(matches!(error, Err(BrozaError::TargetNotFound(_))), "{error:?}");
    }

    #[test]
    fn entries_go_back_in_reverse_sequence_order() {
        let entries = vec![
            entry(1, "/Users/dana/a", 1, ItemStatus::Quarantined),
            entry(2, "/Users/dana/a/child", 1, ItemStatus::Quarantined),
            entry(3, "/Users/dana/b", 1, ItemStatus::Skipped),
        ];
        let wanted = Wanted { session: session_id(), entries: None };

        let order = restore_order(entries, &wanted);

        assert_eq!(order, vec![entry_id(2), entry_id(1)], "a skipped entry has nothing to put back");
    }

    #[test]
    fn identifiers_are_grouped_by_the_session_they_name() {
        let other: EntryId = "cln_20260801091200_c3d4/0009".parse().unwrap_or_else(|e| panic!("{e}"));
        let ids = vec![entry_id(1), other.clone(), entry_id(2)];

        let grouped = group_by_session(&ids).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].session, session_id());
        assert_eq!(grouped[0].entries, Some(vec![entry_id(1), entry_id(2)]));
        assert_eq!(grouped[1].entries, Some(vec![other]));
    }
}
