//! The pre-execution expiry step of `broza clean --apply` (`docs/cli-spec.md` §3.4).
//!
//! Before the plan runs, every quarantine session whose retention period is
//! over is removed, exactly as `broza quarantine expire` would. The step asks
//! at green level (`y/N`, covered by `--yes`); a declined prompt skips the step
//! and says so, it never aborts the cleanup the user already confirmed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use broza::BrozaError;
use broza::model::{ExpiredSession, ItemStatus, Risk, SessionId, Warning};
use broza::ports::{Answer, ConfirmationRequest, Ports};
use broza::quarantine::{expire, expired_sessions, layout, store};
use broza::safety::guard::approve_quarantine_write;
use broza::scan::MountTable;

/// Warning code: sessions are past their retention and a dry run left them.
pub const EXPIRY_PENDING_CODE: &str = "expiry_pending";
/// Warning code: the user declined the expiry step; the sessions stay.
pub const EXPIRY_DECLINED_CODE: &str = "expiry_declined";
/// Warning code: the store could not be read for the expiry step.
pub const EXPIRY_UNREADABLE_CODE: &str = "expiry_unreadable";

/// What the expiry step produced.
#[derive(Debug, Default)]
pub struct Expiry {
    /// Sessions removed, for `data.expired_sessions`.
    pub sessions: Vec<ExpiredSession>,
    /// Bytes the removals freed; they count towards `reclaimed_bytes`.
    pub freed_bytes: u64,
    /// `errors[]` entries: a session that could not be removed.
    pub errors: Vec<Warning>,
    /// `warnings[]` entries.
    pub warnings: Vec<Warning>,
}

/// The sessions a dry run reports as due, with the bytes each holds.
///
/// A store that cannot be read is a warning, not a failure: a dry run must
/// still show the plan.
pub fn due_sessions(ports: &Ports, root: &Path, ttl: Duration) -> (Vec<(SessionId, u64)>, Vec<Warning>) {
    match sessions_due(ports, root, ttl) {
        Ok(due) => {
            let warnings = (!due.is_empty())
                .then(|| {
                    let bytes: u64 = due.iter().map(|(_, bytes)| *bytes).fold(0, u64::saturating_add);
                    Warning {
                        code: EXPIRY_PENDING_CODE.to_owned(),
                        message: format!(
                            "{} quarantine session(s) holding {bytes} bytes are past their retention; `--apply` expires them",
                            due.len()
                        ),
                        path: Some(root.to_path_buf()),
                    }
                })
                .into_iter()
                .collect();
            (due, warnings)
        }
        Err(error) => (Vec::new(), vec![unreadable(root, &error)]),
    }
}

/// Confirm and remove the sessions whose retention is over.
///
/// # Errors
///
/// Only what the guard or the store report as fatal; a session that cannot be
/// removed is an `errors[]` entry inside [`Expiry`].
pub fn expire_due(
    ports: &Ports,
    root: &Path,
    ttl: Duration,
    mounts: &MountTable,
    yes: bool,
) -> Result<Expiry, BrozaError> {
    let due = match sessions_due(ports, root, ttl) {
        Ok(due) => due,
        Err(error) => return Ok(Expiry { warnings: vec![unreadable(root, &error)], ..Expiry::default() }),
    };
    if due.is_empty() {
        return Ok(Expiry::default());
    }
    if !yes && ports.prompter.confirm(&expiry_request(&due)) != Answer::Yes {
        return Ok(Expiry { warnings: vec![declined(&due, root)], ..Expiry::default() });
    }
    let ids: Vec<SessionId> = due.iter().map(|(id, _)| id.clone()).collect();
    let paths = store_paths(ports, root, &ids)?;
    let token = approve_quarantine_write(&paths, root, mounts, ports.fs.as_ref())?;
    let reported = expire(&token, &ids, root, ports.fs.as_ref())?;
    let sessions: Vec<ExpiredSession> = reported
        .data
        .sessions
        .iter()
        .filter(|session| session.status == ItemStatus::Purged)
        .map(|session| ExpiredSession { id: session.id.clone(), freed_bytes: session.total_bytes })
        .collect();
    Ok(Expiry {
        sessions,
        freed_bytes: reported.data.reclaimed_bytes,
        errors: reported.errors,
        warnings: reported.warnings,
    })
}

/// The due sessions with their bytes, newest first as the store lists them.
fn sessions_due(ports: &Ports, root: &Path, ttl: Duration) -> Result<Vec<(SessionId, u64)>, BrozaError> {
    let ids = expired_sessions(root, ports.fs.as_ref(), ports.clock.as_ref(), ttl)?;
    ids.into_iter()
        .map(|id| {
            let found = store::read_one(ports.fs.as_ref(), root, &id)?;
            Ok((id, found.session().total_bytes))
        })
        .collect()
}

/// Every path the removal of `ids` writes to: the stored items and the session directories.
fn store_paths(ports: &Ports, root: &Path, ids: &[SessionId]) -> Result<Vec<PathBuf>, BrozaError> {
    let mut paths = Vec::new();
    for id in ids {
        let found = store::read_one(ports.fs.as_ref(), root, id)?;
        paths.extend(found.session().entries.iter().filter_map(|entry| entry.stored_path.clone()));
        paths.push(layout::session_dir(root, id));
    }
    Ok(paths)
}

/// The green-level prompt for the expiry step.
fn expiry_request(due: &[(SessionId, u64)]) -> ConfirmationRequest {
    ConfirmationRequest {
        max_risk: Risk::Green,
        item_count: due.len(),
        total_bytes: due.iter().map(|(_, bytes)| *bytes).fold(0, u64::saturating_add),
        irreversible: true,
        preview: due.iter().map(|(id, _)| format!("expire quarantine session {id}")).collect(),
    }
}

fn declined(due: &[(SessionId, u64)], root: &Path) -> Warning {
    Warning {
        code: EXPIRY_DECLINED_CODE.to_owned(),
        message: format!(
            "{} expired quarantine session(s) were left in place; `broza quarantine expire` removes them",
            due.len()
        ),
        path: Some(root.to_path_buf()),
    }
}

fn unreadable(root: &Path, error: &BrozaError) -> Warning {
    Warning {
        code: EXPIRY_UNREADABLE_CODE.to_owned(),
        message: format!("the quarantine store could not be read for the expiry step: {error}"),
        path: Some(root.to_path_buf()),
    }
}
