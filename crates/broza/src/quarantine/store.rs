//! Finding the sessions that are in the store and reading their manifests.
//!
//! A session directory is named after its identifier, and the grammar of a
//! [`SessionId`] admits neither `/` nor `.`, so
//! [`layout::session_dir`](crate::quarantine::layout::session_dir) can only ever
//! produce a direct child of the root. That is what keeps `expire` and `purge`
//! from reaching outside the store: they never take a path from the caller, only
//! an identifier.

use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::model::{QuarantineSession, SessionId};
use crate::ports::FileOps;
use crate::quarantine::layout;
use crate::quarantine::manifest::{self, Manifest};

/// One session of the store, with the manifest it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSession {
    /// Identifier of the session.
    pub id: SessionId,
    /// Directory holding it, always a direct child of the store root.
    pub dir: PathBuf,
    /// Content of its `manifest.json`.
    pub manifest: Manifest,
}

impl StoredSession {
    /// The session the manifest describes.
    pub fn session(&self) -> &QuarantineSession {
        &self.manifest.session
    }
}

/// Every session in the store, newest first.
///
/// A store that does not exist yet holds no sessions; a child that is not named
/// like a session, or is not a directory, is not one and is ignored. So is a
/// session directory with no manifest at all: the mover writes the manifest
/// before it moves anything, so such a directory provably holds no items.
///
/// # Errors
///
/// Whatever reading the root reports, and any manifest that exists but cannot
/// be parsed. One corrupt manifest fails the whole listing on purpose: a store
/// Broza cannot fully account for is not one it should offer to delete from.
pub fn read_all(fs: &dyn FileOps, root: &Path) -> Result<Vec<StoredSession>, BrozaError> {
    if !fs.exists(root) {
        return Ok(Vec::new());
    }
    let mut sessions = Vec::new();
    for child in fs.read_dir(root)? {
        let Some(id) = session_id_of(&child) else {
            continue;
        };
        if !fs.metadata(&child)?.is_dir {
            continue;
        }
        match read_one(fs, root, &id) {
            Ok(found) => sessions.push(found),
            Err(BrozaError::TargetNotFound(_)) => {}
            Err(error) => return Err(error),
        }
    }
    sessions.sort_by(|left, right| {
        right.session().created_at.cmp(&left.session().created_at).then_with(|| right.id.cmp(&left.id))
    });
    Ok(sessions)
}

/// The identifiers of every session in the store, newest first.
///
/// # Errors
///
/// See [`read_all`].
pub fn all_ids(fs: &dyn FileOps, root: &Path) -> Result<Vec<SessionId>, BrozaError> {
    Ok(read_all(fs, root)?.into_iter().map(|stored| stored.id).collect())
}

/// One session of the store, by identifier.
///
/// # Errors
///
/// [`BrozaError::TargetNotFound`] for an identifier the store does not hold
/// (exit `4`), and [`BrozaError::Other`] when the manifest is corrupt or names a
/// different session than the directory it sits in.
pub fn read_one(fs: &dyn FileOps, root: &Path, id: &SessionId) -> Result<StoredSession, BrozaError> {
    let dir = layout::session_dir(root, id);
    let manifest = manifest::read(fs, &layout::manifest_path(&dir))?;
    if manifest.session.id != *id {
        return Err(BrozaError::Other(format!(
            "quarantine session `{id}`: its manifest claims to be `{}`",
            manifest.session.id
        )));
    }
    Ok(StoredSession { id: id.clone(), dir, manifest })
}

/// The identifier a session directory is named after, if it is one.
fn session_id_of(path: &Path) -> Option<SessionId> {
    path.file_name()?.to_str()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{all_ids, read_all, read_one};
    use crate::BrozaError;
    use crate::model::{ItemStatus, SessionState};
    use crate::ports::FileOps;
    use crate::quarantine::fixtures::{ROOT, entry, manifest_file, session, session_fs, store_fs};
    use crate::quarantine::manifest::{self, Manifest};
    use crate::testing::FakeFileOps;

    fn with_session(fs: &FakeFileOps) {
        let entries = vec![entry(1, "/Users/dana/Library/Caches/app", 10, ItemStatus::Quarantined)];
        let manifest = Manifest::new(session(SessionState::Complete, entries));
        manifest::write(fs, &manifest_file(), &manifest).unwrap_or_else(|error| panic!("{error}"));
    }

    #[test]
    fn a_store_that_was_never_used_holds_no_sessions() {
        let fs = FakeFileOps::new().with_root("/", 1);

        assert_eq!(read_all(&fs, Path::new(ROOT)).ok(), Some(Vec::new()));
    }

    #[test]
    fn an_empty_store_holds_no_sessions() {
        assert_eq!(read_all(&store_fs(), Path::new(ROOT)).ok(), Some(Vec::new()));
    }

    #[test]
    fn a_session_is_found_by_the_name_of_its_directory() {
        let fs = session_fs();
        with_session(&fs);

        let sessions = read_all(&fs, Path::new(ROOT)).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session().state, SessionState::Complete);
        assert_eq!(all_ids(&fs, Path::new(ROOT)).ok(), Some(vec![sessions[0].id.clone()]));
    }

    #[test]
    fn a_directory_that_is_not_named_like_a_session_is_ignored() {
        let fs = session_fs();
        with_session(&fs);
        fs.add_dir(format!("{ROOT}/scratch"));
        fs.add_file(format!("{ROOT}/README"), b"not a session");

        assert_eq!(read_all(&fs, Path::new(ROOT)).map(|found| found.len()).ok(), Some(1));
    }

    #[test]
    fn a_file_named_like_a_session_is_not_one() {
        let fs = store_fs();
        fs.add_file(format!("{ROOT}/cln_20260801091200_c3d4"), b"impostor");

        assert_eq!(read_all(&fs, Path::new(ROOT)).map(|found| found.len()).ok(), Some(0));
    }

    #[test]
    fn a_session_directory_without_a_manifest_holds_nothing_and_is_ignored() {
        let fs = session_fs();

        assert_eq!(read_all(&fs, Path::new(ROOT)).map(|found| found.len()).ok(), Some(0));
    }

    #[test]
    fn a_corrupt_manifest_fails_the_whole_listing() {
        let fs = session_fs();
        fs.add_file(manifest_file(), b"{");

        assert!(matches!(read_all(&fs, Path::new(ROOT)), Err(BrozaError::Other(_))));
    }

    #[test]
    fn an_unknown_identifier_is_a_missing_target() {
        let fs = store_fs();
        let ghost = "cln_20260801091200_c3d4".parse().unwrap_or_else(|error| panic!("{error}"));

        let error = read_one(&fs, Path::new(ROOT), &ghost);

        assert!(matches!(error, Err(BrozaError::TargetNotFound(_))), "{error:?}");
    }

    #[test]
    fn a_manifest_that_names_another_session_is_refused() {
        let fs = session_fs();
        with_session(&fs);
        let raw = String::from_utf8(fs.read(&manifest_file()).unwrap_or_default()).unwrap_or_default();
        fs.add_file(manifest_file(), raw.replace("_a1b2", "_c3d4").as_bytes());

        let error = read_all(&fs, Path::new(ROOT));

        assert!(matches!(error, Err(BrozaError::Other(_))), "{error:?}");
    }
}
