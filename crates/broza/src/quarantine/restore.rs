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
pub(super) struct Wanted {
    /// The session.
    pub(super) session: SessionId,
    /// The entries to restore; `None` means all of them.
    pub(super) entries: Option<Vec<EntryId>>,
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
pub(super) fn restore_order(entries: Vec<QuarantineEntry>, wanted: &Wanted) -> Vec<EntryId> {
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
        Recheck::Unchanged => fs.remove_tree(&found.dir),
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
pub(super) fn group_by_session(ids: &[EntryId]) -> Result<Vec<Wanted>, BrozaError> {
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
