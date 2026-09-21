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

use std::path::Path;

use crate::BrozaError;
use crate::model::{
    EntryId, ErrorEntry, ItemErrorCode, ItemStatus, OperationKind, QuarantineEntry, RestoreReport,
    RestoreSession, SessionId, SessionState, Warning,
};
use crate::ports::FileOps;
use crate::quarantine::closing::{Closing, close, write_state};
use crate::quarantine::entries::{sequence_in, with_entries};
use crate::quarantine::layout;
use crate::quarantine::lock;
use crate::quarantine::manifest::{self, Manifest};
use crate::quarantine::putback::put_back;
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
    restore_wanted(sources, targets, &[Wanted::whole(session)], root, fs, to)
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
    restore_wanted(sources, targets, &group_by_session(ids)?, root, fs, to)
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
///
/// A session that cannot be read or is busy is an `errors[]` line and the run
/// goes on; the caller is expected to have resolved unknown identifiers before
/// anything is written (`docs/cli-spec.md` §3.5).
///
/// # Errors
///
/// See [`restore_session`].
pub fn restore_wanted(
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
///
/// The session is held for the whole of it. A session another Broza is working
/// on is skipped rather than fought over, and one whose manifest cannot be read
/// is reported and the next session still runs: a restore that already moved
/// files must not abort halfway through the list.
fn restore_one(
    sources: &Approved<QuarantineWrite>,
    targets: &Approved<RestoreWrite>,
    wanted: &Wanted,
    root: &Path,
    fs: &dyn FileOps,
    to: Option<&Path>,
) -> Result<Restored, BrozaError> {
    let found = match store::read_one(fs, root, &wanted.session) {
        Ok(found) => found,
        Err(error) => return Ok(unreadable(&wanted.session, root, &error)),
    };
    let held = match lock::take(fs, &found.dir) {
        lock::Taken::Held(held) => held,
        lock::Taken::Busy => return Ok(busy(&found)),
        lock::Taken::Unavailable(error) => return Ok(unreadable(&wanted.session, root, &error)),
    };
    let manifest = write_state(&found.dir, &found.manifest, SessionState::Restoring, fs)?;
    let order = restore_order(manifest.session.entries.clone(), wanted);
    let pass = order
        .iter()
        .try_fold(Pass::new(manifest), |pass, id| put_one(pass, id, &found, (sources, targets), fs, to))?;
    let touched = in_sequence_order(pass.reported);
    let retryable = touched.iter().any(|entry| entry.status.is_unsuccessful());
    let closing = close(&found, &pass.manifest, sources, fs, retryable)?;
    drop(held);
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
        warnings: [pass.warnings, closing.warnings].concat(),
        restored_bytes: restored_bytes(&touched),
    })
}

/// The report line of a session whose manifest could not be read.
fn unreadable(id: &SessionId, root: &Path, error: &BrozaError) -> Restored {
    let dir = layout::session_dir(root, id);
    Restored {
        session: RestoreSession { id: id.clone(), status: ItemStatus::Failed, items: Vec::new() },
        errors: vec![store::corrupt(&dir, error)],
        warnings: Vec::new(),
        restored_bytes: 0,
    }
}

/// The report line of a session another Broza is working on.
fn busy(found: &StoredSession) -> Restored {
    Restored {
        session: RestoreSession { id: found.id.clone(), status: ItemStatus::Skipped, items: Vec::new() },
        errors: vec![diagnostic(
            ItemErrorCode::SessionBusy.as_str(),
            format!(
                "quarantine session `{}` is being written by another Broza; nothing was restored \
                 from it. Try again when that run has finished.",
                found.id
            ),
            Some(&found.dir),
        )],
        warnings: Vec::new(),
        restored_bytes: 0,
    }
}

/// The manifest as it stands, and what each attempt is reported as.
struct Pass {
    /// The manifest as it was last written.
    manifest: Manifest,
    /// One entry per attempt, in the order they were attempted.
    reported: Vec<QuarantineEntry>,
    /// What the user should know about how the items got back.
    warnings: Vec<Warning>,
}

impl Pass {
    /// A pass that has not attempted anything yet.
    fn new(manifest: Manifest) -> Self {
        Self { manifest, reported: Vec::new(), warnings: Vec::new() }
    }
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
    let Pass { manifest, reported, warnings } = pass;
    let Some(position) = manifest.session.entries.iter().position(|entry| entry.id == *id) else {
        return Ok(Pass { manifest, reported, warnings });
    };
    let entry = manifest.session.entries.get(position).ok_or_else(|| missing(id))?;
    let put = put_back(entry, to, tokens.0.items(), tokens.1, fs)?;
    let reported = [reported, vec![put.entry.clone()]].concat();
    let warnings = [warnings, put.warning.into_iter().collect()].concat();
    if put.entry.status != ItemStatus::Restored {
        return Ok(Pass { manifest, reported, warnings });
    }
    let entries = manifest
        .session
        .entries
        .iter()
        .enumerate()
        .map(|(index, existing)| if index == position { put.entry.clone() } else { existing.clone() })
        .collect();
    let session = with_entries(&manifest.session, entries);
    let updated = manifest.with_session(session);
    manifest::write(fs, &layout::manifest_path(&found.dir), &updated)?;
    Ok(Pass { manifest: updated, reported, warnings })
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
