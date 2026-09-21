//! `broza quarantine expire` and `broza quarantine purge`
//! (`docs/cli-spec.md` §3.8.2, §3.8.3 and §4.6).
//!
//! Both delete whole sessions and differ only in *which* ones: `expire` takes
//! the sessions past their retention period, `purge` takes the ones the user
//! named, at any age. Both are irreversible, so both need an
//! [`Approved<QuarantineWrite>`](crate::safety::guard::QuarantineWrite) and both
//! re-check `(device, inode)` before removing anything.
//!
//! A session directory is always derived from an identifier through [`store`],
//! never taken from the caller, so nothing outside the store root can be
//! reached.
//!
//! # Where the two part company
//!
//! A session holding items its manifest does not list is data Broza moved and
//! then lost track of. `expire` runs unattended — `clean --apply` triggers it —
//! so it refuses such a session and reports it. `purge` is the user typing
//! `PURGE` about a session they named, so it removes everything, orphans
//! included, and says so in `warnings[]`.

use std::path::Path;
use std::time::Duration;

use crate::BrozaError;
use crate::model::{
    ErrorEntry, ItemErrorCode, ItemStatus, OperationKind, ReclaimReport, ReclaimSession, SessionId,
    SessionState, Warning,
};
use crate::ports::{Clock, FileOps};
use crate::quarantine::entries::held_bytes;
use crate::quarantine::guarded::{Recheck, io_code, recheck};
use crate::quarantine::lock;
use crate::quarantine::measure::measure_dir_bytes;
use crate::quarantine::report::{Reported, diagnostic};
use crate::quarantine::store::{self, StoredSession};
use crate::quarantine::{layout, ttl};
use crate::safety::guard::{Approved, QuarantineWrite};

/// The sessions whose retention period is over.
///
/// A session that is being restored is never eligible: it belongs to a run in
/// progress (`docs/cli-spec.md` §3.8.3). One left `in_progress` by an
/// interrupted move *is*, because reading it settles every item it was moving
/// against the filesystem, so what it holds is known.
///
/// # Errors
///
/// Whatever reading the store root reports.
pub fn expired_sessions(
    root: &Path,
    fs: &dyn FileOps,
    clock: &dyn Clock,
    retention: Duration,
) -> Result<Vec<SessionId>, BrozaError> {
    let now = clock.now();
    let mut due = Vec::new();
    for found in store::read_all(fs, root)?.sessions {
        if !is_expirable(&found.session().state) {
            continue;
        }
        if !ttl::is_past_ttl(found.session().created_at, retention, now) {
            continue;
        }
        // A session a Broza is writing is not idle, whatever its age says: with
        // `quarantine-ttl 0` every session is past its time the moment it is
        // created, and the one being filled right now must not be swept away.
        // A lock Broza cannot open is treated like a busy one: `list` reports
        // it; an unattended sweep never guesses.
        if !matches!(lock::take_if_present(fs, &found.dir), lock::Taken::Held(_)) {
            continue;
        }
        due.push(found.id);
    }
    Ok(due)
}

/// `true` for the states an automatic expiry may act on.
fn is_expirable(state: &SessionState) -> bool {
    matches!(state, SessionState::Complete | SessionState::InProgress)
}

/// Every readable session in the store, for `purge --all`.
///
/// # Errors
///
/// Whatever reading the store root reports.
pub fn all_sessions(root: &Path, fs: &dyn FileOps) -> Result<Vec<SessionId>, BrozaError> {
    store::all_ids(fs, root)
}

/// Delete the sessions of `ids` because their retention period is over.
///
/// # Errors
///
/// [`BrozaError::Other`] when the token does not cover a session directory. A
/// session that cannot be read, or must not be removed, is reported in
/// `errors[]`, which makes the run partial (exit `5`) rather than fatal.
pub fn expire(
    token: &Approved<QuarantineWrite>,
    ids: &[SessionId],
    root: &Path,
    fs: &dyn FileOps,
) -> Result<Reported<ReclaimReport>, BrozaError> {
    reclaim(token, ids, root, fs, OperationKind::Expire)
}

/// Delete the sessions of `ids` regardless of their age.
///
/// # Errors
///
/// See [`expire`].
pub fn purge(
    token: &Approved<QuarantineWrite>,
    ids: &[SessionId],
    root: &Path,
    fs: &dyn FileOps,
) -> Result<Reported<ReclaimReport>, BrozaError> {
    reclaim(token, ids, root, fs, OperationKind::Purge)
}

/// What removing one session produced.
struct Removed {
    /// The session line of the report.
    session: ReclaimSession,
    /// The `errors[]` entries, when the session was not removed.
    errors: Vec<ErrorEntry>,
    /// The `warnings[]` entries, for what went beyond the manifest.
    warnings: Vec<Warning>,
    /// Bytes the removal actually freed.
    reclaimed_bytes: u64,
}

/// The shared body of `expire` and `purge`.
fn reclaim(
    token: &Approved<QuarantineWrite>,
    ids: &[SessionId],
    root: &Path,
    fs: &dyn FileOps,
    operation: OperationKind,
) -> Result<Reported<ReclaimReport>, BrozaError> {
    let removed = ids
        .iter()
        .map(|id| remove_session(token, id, root, fs, operation))
        .collect::<Result<Vec<Removed>, BrozaError>>()?;
    let report = ReclaimReport {
        operation,
        reclaimed_bytes: removed.iter().fold(0_u64, |sum, one| sum.saturating_add(one.reclaimed_bytes)),
        sessions: removed.iter().map(|one| one.session.clone()).collect(),
    };
    let errors = removed.iter().flat_map(|one| one.errors.clone()).collect();
    let warnings = removed.into_iter().flat_map(|one| one.warnings).collect();
    Ok(Reported::with(report, errors, warnings))
}

/// Read one session, check it may go, and remove its whole directory.
fn remove_session(
    token: &Approved<QuarantineWrite>,
    id: &SessionId,
    root: &Path,
    fs: &dyn FileOps,
    operation: OperationKind,
) -> Result<Removed, BrozaError> {
    let found = match store::read_one(fs, root, id) {
        Ok(found) => found,
        Err(error) => return Ok(unreadable(id, root, &error)),
    };
    if found.session().state == SessionState::Restoring {
        return Ok(refused(&found, &ItemErrorCode::SessionBusy));
    }
    let held = match lock::take(fs, &found.dir) {
        lock::Taken::Held(held) => held,
        lock::Taken::Busy => return Ok(refused(&found, &ItemErrorCode::SessionBusy)),
        lock::Taken::Unavailable(error) => return Ok(unreadable(id, root, &error)),
    };
    if operation == OperationKind::Expire && !found.is_accounted_for() {
        return Ok(Removed { errors: found.orphan_errors(), ..refused(&found, &orphaned()) });
    }
    let removed = match recheck(token.items(), &found.dir, fs)? {
        Recheck::Refused(code) => refused(&found, &code),
        Recheck::Unchanged => {
            let orphan_bytes = measure_orphans(&found, fs)?;
            match fs.remove_tree(&found.dir) {
                Ok(()) => purged(&found, orphan_bytes),
                Err(error) => refused(&found, &io_code(&error)),
            }
        }
    };
    drop(held);
    Ok(removed)
}

/// What the unlisted items of a session occupy, before they are removed.
///
/// They are freed like everything else in the directory, so they count towards
/// `reclaimed_bytes`: reporting only what the manifest knew about would
/// under-report the space the user got back.
fn measure_orphans(found: &StoredSession, fs: &dyn FileOps) -> Result<u64, BrozaError> {
    found
        .orphans
        .iter()
        .try_fold(0_u64, |sum, path| Ok(sum.saturating_add(measure_dir_bytes(fs, path, None)?)))
}

/// The item error code for a session holding more than it lists.
fn orphaned() -> ItemErrorCode {
    ItemErrorCode::from_token(store::ORPHANED_ITEM)
}

/// The report line of a session whose manifest could not be read.
///
/// Skipped, never removed: a session Broza cannot account for is not one it may
/// delete on the user's behalf, and the error names the directory to look at.
fn unreadable(id: &SessionId, root: &Path, error: &BrozaError) -> Removed {
    let dir = layout::session_dir(root, id);
    Removed {
        session: ReclaimSession {
            id: id.clone(),
            total_bytes: 0,
            item_count: 0,
            status: ItemStatus::Skipped,
        },
        errors: vec![store::corrupt(&dir, error)],
        warnings: Vec::new(),
        reclaimed_bytes: 0,
    }
}

/// The report line of a session that was removed.
fn purged(found: &StoredSession, orphan_bytes: u64) -> Removed {
    let reclaimed_bytes = held_bytes(&found.session().entries).saturating_add(orphan_bytes);
    Removed {
        session: line(found, ItemStatus::Purged, reclaimed_bytes),
        errors: Vec::new(),
        warnings: found.orphans.iter().map(|stored| purged_orphan(stored)).collect(),
        reclaimed_bytes,
    }
}

/// The warning an unlisted item removed with its session earns.
fn purged_orphan(stored: &Path) -> Warning {
    diagnostic(
        store::ORPHANED_ITEM,
        format!("`{}` was purged with its session although no entry listed it", stored.display()),
        Some(stored),
    )
}

/// The report line and the `errors[]` entry of a session that stayed.
fn refused(found: &StoredSession, code: &ItemErrorCode) -> Removed {
    let held = held_bytes(&found.session().entries);
    Removed {
        session: line(found, ItemStatus::Skipped, held),
        errors: vec![diagnostic(
            code.as_str(),
            format!("quarantine session `{}` was not removed: {code}", found.id),
            Some(&found.dir),
        )],
        warnings: Vec::new(),
        reclaimed_bytes: 0,
    }
}

/// One `sessions[]` line of the report.
fn line(found: &StoredSession, status: ItemStatus, total_bytes: u64) -> ReclaimSession {
    ReclaimSession { id: found.id.clone(), total_bytes, item_count: found.session().item_count, status }
}
