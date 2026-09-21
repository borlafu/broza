//! Closing a session once a restore has been through it.
//!
//! The one place that may remove a session directory, and the reason it is its
//! own module: "the manifest lists nothing" and "the directory holds nothing"
//! are different statements, and only the second one licenses a `remove_tree`.

use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::model::{ErrorEntry, QuarantineSession, SessionState, Warning};
use crate::ports::FileOps;
use crate::quarantine::guarded::{Recheck, recheck};
use crate::quarantine::layout;
use crate::quarantine::manifest::{self, Manifest};
use crate::quarantine::reconcile::leftovers;
use crate::quarantine::report::diagnostic;
use crate::quarantine::restore::{SESSION_HAS_UNTRACKED_ITEMS, SESSION_LEFT_BEHIND};
use crate::quarantine::store::StoredSession;
use crate::safety::guard::{Approved, QuarantineWrite};

/// What closing the session produced.
#[derive(Default)]
pub struct Closing {
    /// `errors[]` entries the session as a whole earned.
    pub errors: Vec<ErrorEntry>,
    /// `warnings[]` entries the user should read.
    pub warnings: Vec<Warning>,
}

/// Remove an emptied session, or leave it in a state that can be retried.
///
/// # Errors
///
/// Whatever reading the directory or rewriting the manifest reports.
///
/// The manifest saying "nothing left" is not enough. The directory is read
/// again, and anything still under `items/` — an entry that was lost, a file a
/// crash left behind — keeps the session, is reported, and makes the restore
/// unsuccessful. Deleting a directory that still holds user data because a
/// *manifest* claims it is empty is the one mistake this function exists to
/// prevent.
pub fn close(
    found: &StoredSession,
    manifest: &Manifest,
    sources: &Approved<QuarantineWrite>,
    fs: &dyn FileOps,
    retryable: bool,
) -> Result<Closing, BrozaError> {
    let left = leftovers(fs, &found.dir)?;
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
///
/// # Errors
///
/// Whatever writing the manifest reports.
pub fn write_state(
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
