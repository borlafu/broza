//! `broza restore` (`docs/cli-spec.md` §3.5 and §4.6).
//!
//! One session at a time, entries in reverse sequence order so a child that was
//! moved after its parent goes back first. The manifest is rewritten after every
//! entry that leaves, so an interrupted restore leaves a session in state
//! `restoring` that can simply be run again.
//!
//! # A session is deleted only when the disk agrees
//!
//! "The manifest lists nothing" and "the directory holds nothing" are two
//! different statements, and only the second one licenses a `remove_tree`. The
//! session is re-read from the filesystem before it is dropped; anything still
//! under `items/` keeps it alive and is reported, because a manifest that has
//! lost track of a file is exactly the case where deleting the directory would
//! destroy user data.

use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::model::{
    EntryId, ErrorEntry, ItemStatus, OperationKind, QuarantineEntry, QuarantineSession, RestoreReport,
    RestoreSession, SessionId, SessionState, Warning,
};
use crate::ports::FileOps;
use crate::quarantine::entries::{sequence_in, with_entries};
use crate::quarantine::guarded::{Recheck, recheck};
use crate::quarantine::layout;
use crate::quarantine::manifest::{self, Manifest};
use crate::quarantine::putback::put_back;
use crate::quarantine::reconcile::stored_items;
use crate::quarantine::report::{Reported, diagnostic};
use crate::quarantine::selection::{Wanted, group_by_session, restore_order};
use crate::quarantine::store::{self, StoredSession};
use crate::safety::guard::{Approved, QuarantineWrite, RestoreWrite};

/// Warning code for a session that was emptied but could not be removed.
pub const SESSION_LEFT_BEHIND: &str = "session_left_behind";
/// Error code for a session whose manifest is empty while its directory is not.
pub const SESSION_HAS_UNTRACKED_ITEMS: &str = "session_has_untracked_items";

pub use crate::quarantine::selection::{entry_destinations, session_destinations};

/// Restore every item of one session.
///
/// `sources` authorises moving the stored items; `targets` authorises the
/// places they go back to, which come from [`session_destinations`].
///
/// # Errors
///
/// [`BrozaError::TargetNotFound`] for a session the store does not hold (exit
/// `4`), [`BrozaError::Other`] when either token does not cover a path it is
/// supposed to authorise, and whatever writing the manifest reports.
pub fn restore_session(
    sources: &Approved<QuarantineWrite>,
    targets: &Approved<RestoreWrite>,
    session: &SessionId,
    root: &Path,
    fs: &dyn FileOps,
    to: Option<&Path>,
) -> Result<Reported<RestoreReport>, BrozaError> {
    restore_all(sources, targets, &[Wanted::whole(session)], root, fs, to)
}

/// Restore the named items, which may belong to several sessions.
///
/// # Errors
///
/// See [`restore_session`], plus [`BrozaError::Usage`] when an identifier does
/// not name a session.
pub fn restore_entries(
    sources: &Approved<QuarantineWrite>,
    targets: &Approved<RestoreWrite>,
    ids: &[EntryId],
    root: &Path,
    fs: &dyn FileOps,
    to: Option<&Path>,
) -> Result<Reported<RestoreReport>, BrozaError> {
    restore_all(sources, targets, &group_by_session(ids)?, root, fs, to)
}

/// What restoring one session produced.
#[allow(clippy::struct_field_names, reason = "`restored_bytes` is the name the JSON contract uses")]
struct Restored {
    /// The session line of the report.
    session: RestoreSession,
    /// One `errors[]` entry per item that did not go back.
    errors: Vec<ErrorEntry>,
    /// One `warnings[]` entry per thing the user should look at afterwards.
    warnings: Vec<Warning>,
    /// Bytes moved back.
    restored_bytes: u64,
}

/// Restore each wanted session in turn and merge the reports.
fn restore_all(
    sources: &Approved<QuarantineWrite>,
    targets: &Approved<RestoreWrite>,
    wanted: &[Wanted],
    root: &Path,
    fs: &dyn FileOps,
    to: Option<&Path>,
) -> Result<Reported<RestoreReport>, BrozaError> {
    let done = wanted
        .iter()
        .map(|one| restore_one(sources, targets, one, root, fs, to))
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
    sources: &Approved<QuarantineWrite>,
    targets: &Approved<RestoreWrite>,
    wanted: &Wanted,
    root: &Path,
    fs: &dyn FileOps,
    to: Option<&Path>,
) -> Result<Restored, BrozaError> {
    let found = store::read_one(fs, root, &wanted.session)?;
    let manifest = write_state(&found.dir, &found.manifest, SessionState::Restoring, fs)?;
    let order = restore_order(manifest.session.entries.clone(), wanted);
    let pass = order.iter().try_fold(Pass { manifest, reported: Vec::new() }, |pass, id| {
        put_one(pass, id, &found, (sources, targets), fs, to)
    })?;
    let touched = in_sequence_order(pass.reported);
    let retryable = touched.iter().any(|entry| entry.status.is_unsuccessful());
    let closing = close(&found, &pass.manifest, sources, fs, retryable)?;
    Ok(Restored {
        session: RestoreSession {
            id: found.id.clone(),
            status: session_status(&touched, &closing),
            items: touched.clone(),
        },
        errors: touched
            .iter()
            .filter_map(|entry| failure_of(entry, &found))
            .chain(closing.errors.clone())
            .collect(),
        warnings: closing.warnings,
        restored_bytes: restored_bytes(&touched),
    })
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
    tokens: (&Approved<QuarantineWrite>, &Approved<RestoreWrite>),
    fs: &dyn FileOps,
    to: Option<&Path>,
) -> Result<Pass, BrozaError> {
    let Pass { manifest, reported } = pass;
    let Some(position) = manifest.session.entries.iter().position(|entry| entry.id == *id) else {
        return Ok(Pass { manifest, reported });
    };
    let entry = manifest.session.entries.get(position).ok_or_else(|| missing(id))?;
    let put = put_back(entry, to, tokens.0.items(), tokens.1, fs)?;
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

/// What closing the session produced.
#[derive(Default)]
struct Closing {
    /// `errors[]` entries the session as a whole earned.
    errors: Vec<ErrorEntry>,
    /// `warnings[]` entries the user should read.
    warnings: Vec<Warning>,
}

/// Remove an emptied session, or leave it in a state that can be retried.
///
/// The manifest saying "nothing left" is not enough. The directory is read
/// again, and anything still under `items/` — an entry that was lost, a file a
/// crash left behind — keeps the session, is reported, and makes the restore
/// unsuccessful. Deleting a directory that still holds user data because a
/// *manifest* claims it is empty is the one mistake this function exists to
/// prevent.
fn close(
    found: &StoredSession,
    manifest: &Manifest,
    sources: &Approved<QuarantineWrite>,
    fs: &dyn FileOps,
    retryable: bool,
) -> Result<Closing, BrozaError> {
    let left = stored_items(fs, &found.dir)?;
    let untracked_items = unlisted(&left, manifest);
    if !untracked_items.is_empty() {
        write_state(&found.dir, manifest, SessionState::Restoring, fs)?;
        return Ok(Closing { errors: vec![untracked(found, &untracked_items)], ..Closing::default() });
    }
    if !left.is_empty() {
        // Items the manifest still lists: an entry that could not go back, or
        // one this run was not asked about. Only the first is worth retrying.
        let state = if retryable { SessionState::Restoring } else { SessionState::Complete };
        write_state(&found.dir, manifest, state, fs)?;
        return Ok(Closing::default());
    }
    match drop_session(found, sources, fs) {
        Ok(()) => Ok(Closing::default()),
        Err(error) => {
            write_state(&found.dir, manifest, SessionState::Complete, fs)?;
            Ok(Closing { warnings: vec![left_behind(found, &error)], ..Closing::default() })
        }
    }
}

/// The stored items no entry of `manifest` accounts for.
fn unlisted(left: &[PathBuf], manifest: &Manifest) -> Vec<PathBuf> {
    left.iter()
        .filter(|stored| {
            !manifest.session.entries.iter().any(|entry| entry.stored_path.as_ref() == Some(*stored))
        })
        .cloned()
        .collect()
}

/// Re-check the session directory against the token and remove it.
fn drop_session(
    found: &StoredSession,
    sources: &Approved<QuarantineWrite>,
    fs: &dyn FileOps,
) -> Result<(), BrozaError> {
    match recheck(sources.items(), &found.dir, fs)? {
        Recheck::Unchanged => fs.remove_tree(&found.dir),
        Recheck::Refused(code) => {
            Err(BrozaError::Other(format!("the directory changed since it was checked ({code})")))
        }
    }
}

/// The error a session whose manifest lost track of its contents earns.
fn untracked(found: &StoredSession, left: &[PathBuf]) -> ErrorEntry {
    diagnostic(
        SESSION_HAS_UNTRACKED_ITEMS,
        format!(
            "quarantine session `{}` still holds {} item(s) its manifest does not list, so it was \
             kept; inspect `{}` and remove it with `broza quarantine purge {}` once it is empty.",
            found.id,
            left.len(),
            found.dir.display(),
            found.id
        ),
        Some(&found.dir),
    )
}

/// The warning an emptied session that could not be removed earns.
fn left_behind(found: &StoredSession, error: &BrozaError) -> Warning {
    diagnostic(
        SESSION_LEFT_BEHIND,
        format!("quarantine session `{}` is empty but was not removed: {error}", found.id),
        Some(&found.dir),
    )
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
    items.sort_by_key(|entry| sequence_in(entry).unwrap_or_default());
    items
}

/// The outcome of the session as a whole.
///
/// A session that could not be closed is never reported as restored, however
/// well the individual items went: something is still in the store.
fn session_status(items: &[QuarantineEntry], closing: &Closing) -> ItemStatus {
    if items.iter().any(|entry| entry.status == ItemStatus::Failed) || !closing.errors.is_empty() {
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
        format!(
            "`{}` of session `{}` was not restored: {code}. It is still in quarantine; retry with \
             `broza restore --session {}`.",
            entry.id, found.id, found.id
        ),
        Some(&entry.original_path),
    ))
}

/// An entry that disappeared from the manifest between two reads.
fn missing(id: &EntryId) -> BrozaError {
    BrozaError::Other(format!("quarantine restore: entry `{id}` is no longer in the manifest"))
}
