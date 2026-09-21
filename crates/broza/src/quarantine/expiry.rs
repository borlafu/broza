//! `broza quarantine expire` and `broza quarantine purge`
//! (`docs/cli-spec.md` §3.8.2, §3.8.3 and §4.6).
//!
//! Both delete whole sessions and differ only in *which* ones: `expire` takes
//! the sessions past their retention period, `purge` takes the ones the user
//! named, at any age. Both are irreversible, so both need an
//! [`Approved<QuarantineWrite>`](crate::safety::guard::QuarantineWrite) and both
//! re-check `(device, inode)` before removing anything.
//!
//! A session directory is always derived from an identifier through
//! [`store`](crate::quarantine::store), never taken from the caller, so nothing
//! outside the store root can be reached.

use std::path::Path;
use std::time::Duration;

use crate::BrozaError;
use crate::model::{
    ErrorEntry, ItemErrorCode, ItemStatus, OperationKind, ReclaimReport, ReclaimSession, SessionId,
    SessionState,
};
use crate::ports::{Clock, FileOps};
use crate::quarantine::entries::held_bytes;
use crate::quarantine::guarded::{Recheck, io_code, recheck};
use crate::quarantine::report::{Reported, diagnostic};
use crate::quarantine::store::{self, StoredSession};
use crate::quarantine::ttl;
use crate::safety::guard::{Approved, QuarantineWrite};

/// The sessions whose retention period is over.
///
/// A session is eligible only when it is `complete`: one still `in_progress` or
/// `restoring` belongs to a run that has not finished, and one in a state this
/// Broza does not know is left for the version that does.
///
/// # Errors
///
/// Whatever reading the store reports.
pub fn expired_sessions(
    root: &Path,
    fs: &dyn FileOps,
    clock: &dyn Clock,
    retention: Duration,
) -> Result<Vec<SessionId>, BrozaError> {
    let now = clock.now();
    Ok(store::read_all(fs, root)?
        .into_iter()
        .filter(|found| found.session().state == SessionState::Complete)
        .filter(|found| ttl::is_past_ttl(found.session().created_at, retention, now))
        .map(|found| found.id)
        .collect())
}

/// Every session in the store, for `purge --all`.
///
/// # Errors
///
/// Whatever reading the store reports.
pub fn all_sessions(root: &Path, fs: &dyn FileOps) -> Result<Vec<SessionId>, BrozaError> {
    store::all_ids(fs, root)
}

/// Delete the sessions of `ids` because their retention period is over.
///
/// # Errors
///
/// [`BrozaError::TargetNotFound`] for an identifier the store does not hold
/// (exit `4`), and [`BrozaError::Other`] when the token does not cover a session
/// directory. A session that cannot be removed is reported in `errors[]`.
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
    /// The `errors[]` entry, when the session was not removed.
    error: Option<ErrorEntry>,
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
        .map(|id| remove_session(token, id, root, fs))
        .collect::<Result<Vec<Removed>, BrozaError>>()?;
    let report = ReclaimReport {
        operation,
        reclaimed_bytes: removed.iter().fold(0_u64, |sum, one| sum.saturating_add(one.reclaimed_bytes)),
        sessions: removed.iter().map(|one| one.session.clone()).collect(),
    };
    let errors = removed.into_iter().filter_map(|one| one.error).collect();
    Ok(Reported::with(report, errors, Vec::new()))
}

/// Read one session, check it may go, and remove its whole directory.
fn remove_session(
    token: &Approved<QuarantineWrite>,
    id: &SessionId,
    root: &Path,
    fs: &dyn FileOps,
) -> Result<Removed, BrozaError> {
    let found = store::read_one(fs, root, id)?;
    if found.session().state == SessionState::Restoring {
        return Ok(refused(&found, &ItemErrorCode::SessionBusy));
    }
    match recheck(token.items(), &found.dir, fs)? {
        Recheck::Refused(code) => Ok(refused(&found, &code)),
        Recheck::Approved => Ok(match fs.remove_tree(&found.dir) {
            Ok(()) => purged(&found),
            Err(error) => refused(&found, &io_code(&error)),
        }),
    }
}

/// The report line of a session that was removed.
fn purged(found: &StoredSession) -> Removed {
    let reclaimed_bytes = held_bytes(&found.session().entries);
    Removed { session: line(found, ItemStatus::Purged, reclaimed_bytes), error: None, reclaimed_bytes }
}

/// The report line and the `errors[]` entry of a session that stayed.
fn refused(found: &StoredSession, code: &ItemErrorCode) -> Removed {
    let held = held_bytes(&found.session().entries);
    Removed {
        session: line(found, ItemStatus::Skipped, held),
        error: Some(diagnostic(
            code.as_str(),
            format!("quarantine session `{}` was not removed: {code}", found.id),
            Some(&found.dir),
        )),
        reclaimed_bytes: 0,
    }
}

/// One `sessions[]` line of the report.
fn line(found: &StoredSession, status: ItemStatus, total_bytes: u64) -> ReclaimSession {
    ReclaimSession { id: found.id.clone(), total_bytes, item_count: found.session().item_count, status }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::{all_sessions, expire, expired_sessions, purge};
    use crate::BrozaError;
    use crate::model::{ItemStatus, OperationKind, SessionId, SessionState};
    use crate::ports::FileOps;
    use crate::quarantine::fixtures::{
        NOW, ROOT, at, entry, manifest_file, session, session_dir, session_fs, session_id,
    };
    use crate::quarantine::manifest::{self, Manifest};
    use crate::safety::guard::{Approved, QuarantineWrite, approve_quarantine_write};
    use crate::testing::{FakeFileOps, FixedClock, mac_mount_table};

    const TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
    const AFTER_TTL: &str = "2026-11-01T00:00:00Z";

    fn write_session(fs: &FakeFileOps, state: SessionState) {
        fs.add_dir(session_dir());
        let entries = vec![
            entry(1, "/Users/dana/Library/Caches/app", 10, ItemStatus::Quarantined),
            entry(2, "/Volumes/External/x", 60, ItemStatus::Skipped),
        ];
        let manifest = Manifest::new(session(state, entries));
        manifest::write(fs, &manifest_file(), &manifest).unwrap_or_else(|error| panic!("{error}"));
        fs.add_file(session_dir().join("items/0001/app"), b"content");
    }

    fn stored(state: SessionState) -> FakeFileOps {
        let fs = session_fs();
        write_session(&fs, state);
        fs
    }

    fn token(fs: &FakeFileOps) -> Approved<QuarantineWrite> {
        approve_quarantine_write(&[session_dir()], Path::new(ROOT), &mac_mount_table(), fs)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn ids() -> Vec<SessionId> {
        vec![session_id()]
    }

    #[test]
    fn only_a_complete_session_past_its_retention_period_expires() {
        let fs = stored(SessionState::Complete);
        let before = FixedClock::at(at(NOW));
        let after = FixedClock::at(at(AFTER_TTL));

        assert_eq!(expired_sessions(Path::new(ROOT), &fs, &before, TTL).ok(), Some(Vec::new()));
        assert_eq!(expired_sessions(Path::new(ROOT), &fs, &after, TTL).ok(), Some(ids()));
    }

    #[test]
    fn an_unfinished_session_never_expires_on_its_own() {
        let fs = stored(SessionState::InProgress);
        let after = FixedClock::at(at(AFTER_TTL));

        assert_eq!(expired_sessions(Path::new(ROOT), &fs, &after, TTL).ok(), Some(Vec::new()));
        assert_eq!(all_sessions(Path::new(ROOT), &fs).ok(), Some(ids()), "but `purge --all` still sees it");
    }

    #[test]
    fn expiring_a_session_removes_its_whole_directory_and_frees_its_bytes() {
        let fs = stored(SessionState::Complete);

        let reported =
            expire(&token(&fs), &ids(), Path::new(ROOT), &fs).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(reported.data.operation, OperationKind::Expire);
        assert_eq!(reported.data.reclaimed_bytes, 10, "only the entries it held count");
        assert_eq!(reported.data.sessions[0].status, ItemStatus::Purged);
        assert!(!fs.exists(&session_dir()));
        assert!(fs.exists(Path::new(ROOT)), "the store itself stays");
        assert!(!reported.is_partial());
    }

    #[test]
    fn purging_uses_the_same_shape_with_its_own_operation() {
        let fs = stored(SessionState::Complete);

        let reported =
            purge(&token(&fs), &ids(), Path::new(ROOT), &fs).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(reported.data.operation, OperationKind::Purge);
        assert_eq!(reported.data.sessions[0].item_count, 2);
        assert!(!fs.exists(&session_dir()));
    }

    #[test]
    fn a_session_being_restored_is_refused_and_reported() {
        let fs = stored(SessionState::Restoring);

        let reported =
            purge(&token(&fs), &ids(), Path::new(ROOT), &fs).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(reported.data.sessions[0].status, ItemStatus::Skipped);
        assert_eq!(reported.data.reclaimed_bytes, 0);
        assert_eq!(reported.errors[0].code, "session_busy");
        assert!(reported.is_partial(), "a refused session means exit 5");
        assert!(fs.exists(&session_dir()));
    }

    #[test]
    fn a_session_directory_that_changed_since_the_check_is_left_alone() {
        let fs = stored(SessionState::Complete);
        let approval = token(&fs);
        fs.remove_tree(&session_dir()).unwrap_or_else(|error| panic!("{error}"));
        write_session(&fs, SessionState::Complete);

        let reported =
            purge(&approval, &ids(), Path::new(ROOT), &fs).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(reported.data.sessions[0].status, ItemStatus::Skipped);
        assert!(reported.is_partial());
        assert!(fs.exists(&session_dir()), "a directory that is not the approved one is never removed");
    }

    #[test]
    fn an_unknown_session_is_a_missing_target() {
        let fs = stored(SessionState::Complete);
        let ghost = "cln_20260801091200_c3d4".parse().unwrap_or_else(|error| panic!("{error}"));

        let error = purge(&token(&fs), &[ghost], Path::new(ROOT), &fs);

        assert!(matches!(error, Err(BrozaError::TargetNotFound(_))), "{error:?}");
    }

    #[test]
    fn a_session_the_token_does_not_cover_stops_the_operation() {
        let fs = stored(SessionState::Complete);
        let empty = approve_quarantine_write(&[], Path::new(ROOT), &mac_mount_table(), &fs)
            .unwrap_or_else(|error| panic!("{error}"));

        let error = purge(&empty, &ids(), Path::new(ROOT), &fs);

        assert!(matches!(error, Err(BrozaError::Other(_))), "{error:?}");
        assert!(fs.exists(&session_dir()));
    }
}
