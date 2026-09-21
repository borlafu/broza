//! The pre-execution expiry step of `broza clean --apply` (`docs/cli-spec.md` §3.4).
//!
//! Before the plan runs, every quarantine session whose retention period is
//! over is removed, exactly as `broza quarantine expire` would. The step asks
//! at green level (`y/N`, covered by `--yes`); a declined prompt skips the step
//! and says so, it never aborts the cleanup the user already confirmed.
//!
//! A dry run only *lists* what is due, but finding out takes each due session's
//! `.lock` for an instant (never creating one), so a `quarantine expire` running
//! at the same moment may report that session `session_busy` and skip it.

use std::path::Path;
use std::time::Duration;

use broza::BrozaError;
use broza::model::{ExpiredSession, ItemStatus, Risk, SessionId, Warning};
use broza::ports::{Answer, ConfirmationRequest, Ports};
use broza::quarantine::{expire, expired_sessions};
use broza::scan::MountTable;

use crate::commands::store::{session_dirs_token, session_sizes};

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
/// Never fatal once the user has confirmed the plan: a store that cannot be
/// read, a token the guard refuses or a session that cannot be removed all end
/// up inside [`Expiry`] as warnings or `errors[]`, and the plan still runs.
///
/// # Errors
///
/// None today; the signature stays fallible for a future step that must abort.
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
    // From here on nothing may abort the cleanup the user already confirmed:
    // a store that changed under us (another Broza, a file removed out of
    // band) costs the expiry step, never the plan.
    let reported = match session_dirs_token(ports, root, mounts, &ids)
        .and_then(|token| expire(&token, &ids, root, ports.fs.as_ref()))
    {
        Ok(reported) => reported,
        Err(error) => return Ok(Expiry { warnings: vec![unreadable(root, &error)], ..Expiry::default() }),
    };
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
    session_sizes(ports, root, &ids)
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
