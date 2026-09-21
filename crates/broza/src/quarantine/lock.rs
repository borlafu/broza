//! Holding a session while something is written to it.
//!
//! Two Brozas can run at once — a `clean --apply` in one terminal and a
//! `quarantine purge` in another — and the store is shared mutable state. Every
//! operation that writes to a session holds `<session dir>/.lock` for its whole
//! duration, and every operation that would disturb one takes the same lock
//! *without waiting*: a session someone else is working on is skipped with
//! `session_busy`, never half-moved and never removed.
//!
//! The lock is advisory `flock`, so it only binds Brozas. That is the scope it
//! needs: the risk is one Broza deleting what another is in the middle of
//! moving, not a user with `rm`.

use std::path::Path;

use crate::BrozaError;
use crate::ports::{FileOps, FsLock, is_busy};
use crate::quarantine::layout;

/// What trying to take a session lock produced.
pub enum Taken {
    /// The session is ours until this value is dropped.
    Held(Box<dyn FsLock>),
    /// Another Broza is working on it.
    Busy,
}

impl Taken {
    /// `true` when somebody else holds the session.
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Busy)
    }
}

/// Take the lock of the session in `dir`, creating the lock file if needed.
///
/// # Errors
///
/// Anything except "somebody else holds it", which is [`Taken::Busy`].
pub fn take(fs: &dyn FileOps, dir: &Path) -> Result<Taken, BrozaError> {
    match fs.lock_exclusive(&layout::lock_path(dir)) {
        Ok(held) => Ok(Taken::Held(held)),
        Err(error) if is_busy(&error) => Ok(Taken::Busy),
        Err(error) => Err(error),
    }
}

/// Take the lock only when the lock file is already there.
///
/// `quarantine list` reads and writes nothing, and must stay that way: a store
/// nobody has ever written to has no lock file, and creating one to find out
/// that it is free would be a write. Every operation that *does* write creates
/// the file before it starts, so "no lock file" means "nobody is working here".
///
/// # Errors
///
/// See [`take`].
pub fn take_if_present(fs: &dyn FileOps, dir: &Path) -> Result<Taken, BrozaError> {
    let path = layout::lock_path(dir);
    if !fs.exists(&path) {
        return Ok(Taken::Held(Box::new(Unlocked)));
    }
    take(fs, dir)
}

/// The "lock" of a session no writer has ever touched: nothing to release.
struct Unlocked;

impl FsLock for Unlocked {}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{take, take_if_present};
    use crate::quarantine::fixtures::{ROOT, session_dir, session_fs};
    use crate::quarantine::layout;

    #[test]
    fn the_first_taker_holds_the_session_and_the_second_is_told_it_is_busy() {
        let fs = session_fs();

        let first = take(&fs, &session_dir()).unwrap_or_else(|error| panic!("{error}"));
        let second = take(&fs, &session_dir()).unwrap_or_else(|error| panic!("{error}"));

        assert!(!first.is_busy());
        assert!(second.is_busy());
        assert!(fs.is_locked(layout::lock_path(&session_dir())));
    }

    #[test]
    fn dropping_the_guard_releases_the_session() {
        let fs = session_fs();

        drop(take(&fs, &session_dir()).unwrap_or_else(|error| panic!("{error}")));

        assert!(!fs.is_locked(layout::lock_path(&session_dir())));
        assert!(!take(&fs, &session_dir()).unwrap_or_else(|error| panic!("{error}")).is_busy());
    }

    #[test]
    fn taking_a_lock_that_is_not_there_writes_nothing() {
        let fs = session_fs();
        let before = fs.paths();

        let taken = take_if_present(&fs, &session_dir()).unwrap_or_else(|error| panic!("{error}"));

        assert!(!taken.is_busy(), "nobody is working here");
        assert_eq!(fs.paths(), before, "and no lock file was created to find out");
    }

    #[test]
    fn taking_a_lock_that_is_there_reports_the_holder() {
        let fs = session_fs();
        let _held = take(&fs, &session_dir()).unwrap_or_else(|error| panic!("{error}"));

        let taken = take_if_present(&fs, &session_dir()).unwrap_or_else(|error| panic!("{error}"));

        assert!(taken.is_busy());
    }

    #[test]
    fn a_lock_that_cannot_be_taken_at_all_is_an_error_not_a_busy_session() {
        let fs = session_fs();
        fs.add_denied(session_dir());

        let error = take(&fs, &session_dir());

        assert!(error.is_err(), "a session Broza cannot even open is not simply busy");
    }

    #[test]
    fn a_lock_lives_inside_the_session_it_names() {
        assert_eq!(layout::lock_path(&session_dir()), session_dir().join(".lock"));
        assert!(layout::lock_path(&session_dir()).starts_with(Path::new(ROOT)));
    }
}
