//! `broza quarantine list` (`docs/cli-spec.md` §3.8.1 and §4.5).
//!
//! Reading only: nothing here writes, so no token is involved. `entries` are
//! dropped from every session, because the listing reports sessions and
//! `restore --list` reports items.

use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;

use crate::BrozaError;
use crate::model::{QuarantineList, QuarantineSession, SessionState, Warning};
use crate::ports::{Clock, FileOps};
use crate::quarantine::entries::held_bytes;
use crate::quarantine::layout;
use crate::quarantine::measure::measure_dir_bytes;
use crate::quarantine::report::{Reported, diagnostic};
use crate::quarantine::store::StoredSession;
use crate::quarantine::{lock, store, ttl};

/// Warning code for a session a `clean --apply` never finished.
pub const SESSION_INCOMPLETE: &str = "session_incomplete";
/// Warning code for a session holding more than its manifest accounts for.
pub const UNTRACKED_BYTES: &str = "untracked_bytes";
/// Warning code for a session another Broza is writing right now.
pub const SESSION_BUSY: &str = "session_busy";
/// Warning: the session's lock file cannot be opened by this Broza.
pub const LOCK_UNREADABLE: &str = "lock_unreadable";

/// Every session in the store, newest first.
///
/// `expires_at` is recomputed from `created_at` plus the *current*
/// `quarantine-ttl`, and `state` becomes `expired` when that instant has passed
/// — the derived state of `docs/cli-spec.md` §4.5, which no manifest stores.
///
/// A session whose manifest cannot be read is skipped and reported in
/// `errors[]`, so one bad session never hides the rest of the store.
///
/// # Errors
///
/// Only what reading the store root itself reports; see [`store::read_all`].
pub fn list_sessions(
    root: &Path,
    fs: &dyn FileOps,
    clock: &dyn Clock,
    retention: Duration,
) -> Result<Reported<QuarantineList>, BrozaError> {
    let now = clock.now();
    let found = store::read_all(fs, root)?;
    let sessions = found
        .sessions
        .iter()
        .map(|stored| summarise(stored, retention, now, fs))
        .collect::<Result<Vec<QuarantineSession>, BrozaError>>()?;
    let warnings = found.sessions.iter().flat_map(|stored| session_warnings(stored, fs)).collect::<Vec<_>>();
    let list = QuarantineList {
        quarantine_path: root.to_path_buf(),
        total_bytes: sum_bytes(&sessions, |_| true),
        expired_bytes: sum_bytes(&sessions, |session| session.state == SessionState::Expired),
        sessions,
    };
    Ok(Reported::with(list, found.errors, warnings))
}

/// One session as the listing reports it: no entries, derived state and expiry.
///
/// `total_bytes` is what the manifest accounts for **plus** what the session
/// holds that it does not: an orphan occupies the disk whether or not anything
/// lists it, and a total that ignored it would be a lie about reclaimable space
/// (`AGENTS.md` §2.7).
fn summarise(
    stored: &StoredSession,
    retention: Duration,
    now: Timestamp,
    fs: &dyn FileOps,
) -> Result<QuarantineSession, BrozaError> {
    let session = stored.session();
    let expires_at = ttl::expires_at(session.created_at, retention);
    Ok(QuarantineSession {
        expires_at,
        total_bytes: held_bytes(&session.entries).saturating_add(orphan_bytes(stored, fs)?),
        state: displayed_state(&session.state, expires_at, now),
        entries: Vec::new(),
        ..session.clone()
    })
}

/// Bytes the session holds that no entry accounts for.
///
/// Only the orphans are measured. Walking every stored item on every `list`
/// would re-measure trees the manifest already describes, which is the one
/// thing a read-only command must not cost.
fn orphan_bytes(stored: &StoredSession, fs: &dyn FileOps) -> Result<u64, BrozaError> {
    stored
        .orphans
        .iter()
        .try_fold(0_u64, |sum, path| Ok(sum.saturating_add(measure_dir_bytes(fs, path, None)?)))
}

/// The warnings one session earns.
///
/// A session another Broza is writing is still listed — its manifest is
/// written atomically, so what is read is a complete document — with a warning
/// that the figures are a snapshot of something in motion. Hiding it would be
/// the one thing worse than showing it: the user would think the space was
/// already gone.
fn session_warnings(stored: &StoredSession, fs: &dyn FileOps) -> Vec<Warning> {
    let mut warnings = Vec::new();
    match lock::take_if_present(fs, &stored.dir) {
        lock::Taken::Held(_) => {}
        lock::Taken::Busy => warnings.push(busy(stored)),
        lock::Taken::Unavailable(error) => warnings.push(unlockable(stored, &error)),
    }
    if stored.session().state == SessionState::InProgress {
        warnings.push(incomplete(stored.id.as_str(), &stored.dir));
    }
    if !stored.is_accounted_for() {
        warnings.push(untracked(stored));
    }
    warnings
}

/// The warning a session whose lock Broza cannot open earns.
///
/// Typically a root-owned `.lock` left behind by `sudo broza`. The session is
/// still listed; nothing automatic will touch it until the lock is readable.
fn unlockable(stored: &StoredSession, error: &BrozaError) -> Warning {
    diagnostic(
        LOCK_UNREADABLE,
        format!(
            "session `{}` has a lock file Broza cannot open ({error}); it is listed but no \
             automatic step will touch it. Fix its permissions or remove `{}`.",
            stored.id,
            layout::lock_path(&stored.dir).display()
        ),
        Some(&stored.dir),
    )
}

/// The warning a session someone is working on earns.
fn busy(stored: &StoredSession) -> Warning {
    diagnostic(
        SESSION_BUSY,
        format!(
            "session `{}` is being written by another Broza; its size and state are a snapshot \
             of a run in progress.",
            stored.id
        ),
        Some(&stored.dir),
    )
}

/// The warning a session holding unaccounted items earns.
fn untracked(stored: &StoredSession) -> Warning {
    diagnostic(
        UNTRACKED_BYTES,
        format!(
            "session `{}` holds {} item(s) its manifest does not list; they are counted in its size \
             and no automatic step will remove them.",
            stored.id,
            stored.orphans.len()
        ),
        Some(&stored.dir),
    )
}

/// `expired` replaces `complete` once the retention period is over.
///
/// Every other state is reported as it was stored, unknown tokens included: a
/// session a newer Broza put in a state this one does not know is still that
/// session, and rewriting its state would lose information.
fn displayed_state(state: &SessionState, expires_at: Timestamp, now: Timestamp) -> SessionState {
    if *state == SessionState::Complete && expires_at < now {
        return SessionState::Expired;
    }
    state.clone()
}

/// Sum the sizes of the sessions a predicate selects.
fn sum_bytes(sessions: &[QuarantineSession], selected: impl Fn(&QuarantineSession) -> bool) -> u64 {
    sessions
        .iter()
        .filter(|session| selected(session))
        .fold(0_u64, |sum, session| sum.saturating_add(session.total_bytes))
}

/// The warning an unfinished session earns.
fn incomplete(id: &str, dir: &Path) -> Warning {
    diagnostic(
        SESSION_INCOMPLETE,
        format!(
            "session `{id}` is not marked finished: a clean may still be running. If none is, \
             restore it or remove it with `broza quarantine purge {id}`."
        ),
        Some(dir),
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::{SESSION_INCOMPLETE, list_sessions};
    use crate::model::{ItemStatus, QuarantineList, SessionState};
    use crate::quarantine::fixtures::{NOW, ROOT, at, entry, manifest_file, session, session_fs};
    use crate::quarantine::manifest::{self, Manifest};
    use crate::testing::{FakeFileOps, FixedClock};

    const TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

    fn stored(state: SessionState) -> FakeFileOps {
        let fs = session_fs();
        let entries = vec![
            entry(1, "/Users/dana/Library/Caches/app", 10, ItemStatus::Quarantined),
            entry(2, "/Volumes/External/x", 60, ItemStatus::Skipped),
        ];
        let manifest = Manifest::new(session(state, entries));
        manifest::write(&fs, &manifest_file(), &manifest).unwrap_or_else(|error| panic!("{error}"));
        fs
    }

    fn listed(fs: &FakeFileOps, now: &str) -> QuarantineList {
        let clock = FixedClock::at(at(now));
        list_sessions(Path::new(ROOT), fs, &clock, TTL).unwrap_or_else(|error| panic!("{error}")).data
    }

    #[test]
    fn a_listing_reports_the_store_and_the_bytes_it_holds() {
        let list = listed(&stored(SessionState::Complete), NOW);

        assert_eq!(list.quarantine_path, Path::new(ROOT));
        assert_eq!(list.total_bytes, 10, "only the entries that were moved hold bytes");
        assert_eq!(list.expired_bytes, 0);
        assert_eq!(list.sessions.len(), 1);
        assert_eq!(list.sessions[0].item_count, 2);
    }

    #[test]
    fn a_listing_omits_the_entries_of_a_session() {
        let list = listed(&stored(SessionState::Complete), NOW);

        assert!(list.sessions[0].entries.is_empty(), "entries belong to `restore --list`");
    }

    #[test]
    fn expiry_is_derived_from_the_current_retention_period() {
        let list = listed(&stored(SessionState::Complete), NOW);

        assert_eq!(list.sessions[0].expires_at, at("2026-10-21T10:36:08Z"));
        assert_eq!(list.sessions[0].state, SessionState::Complete);
    }

    #[test]
    fn a_session_past_its_retention_period_is_reported_as_expired() {
        let list = listed(&stored(SessionState::Complete), "2026-11-01T00:00:00Z");

        assert_eq!(list.sessions[0].state, SessionState::Expired);
        assert_eq!(list.expired_bytes, 10);
        assert_eq!(list.total_bytes, 10);
    }

    #[test]
    fn an_unfinished_session_is_listed_with_a_warning() {
        let reported =
            list_sessions(Path::new(ROOT), &stored(SessionState::InProgress), &FixedClock::at(at(NOW)), TTL)
                .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(reported.data.sessions[0].state, SessionState::InProgress);
        assert_eq!(reported.warnings.len(), 1);
        assert_eq!(reported.warnings[0].code, SESSION_INCOMPLETE);
        assert!(!reported.is_partial(), "a warning never changes the exit code");
    }

    #[test]
    fn an_unfinished_session_is_never_reported_as_expired() {
        let list = listed(&stored(SessionState::InProgress), "2030-01-01T00:00:00Z");

        assert_eq!(list.sessions[0].state, SessionState::InProgress);
        assert_eq!(list.expired_bytes, 0);
    }

    #[test]
    fn a_state_from_a_newer_broza_is_listed_as_it_was_stored() {
        let list = listed(&stored(SessionState::Unknown("archived".to_owned())), "2030-01-01T00:00:00Z");

        assert_eq!(list.sessions[0].state, SessionState::Unknown("archived".to_owned()));
    }

    #[test]
    fn an_empty_store_lists_nothing_and_warns_about_nothing() {
        let reported = list_sessions(Path::new(ROOT), &session_fs(), &FixedClock::at(at(NOW)), TTL)
            .unwrap_or_else(|error| panic!("{error}"));

        assert!(reported.data.sessions.is_empty());
        assert_eq!((reported.data.total_bytes, reported.data.expired_bytes), (0, 0));
        assert!(reported.warnings.is_empty());
    }
}
