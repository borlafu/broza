//! Finding the sessions that are in the store and reading their manifests.
//!
//! A session directory is named after its identifier, and the grammar of a
//! [`SessionId`] admits neither `/` nor `.`, so [`layout::session_dir`] can only
//! ever produce a direct child of the root. That is what keeps `expire` and
//! `purge` from reaching outside the store: they never take a path from the
//! caller, only an identifier.
//!
//! Every read is reconciled against the filesystem
//! ([`mod@crate::quarantine::reconcile`]), and one unreadable session
//! never hides the rest: it becomes an `errors[]` entry and the others are
//! still listed.

use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::model::{ErrorEntry, QuarantineSession, SessionId};
use crate::ports::FileOps;
use crate::quarantine::layout;
use crate::quarantine::manifest::{self, Manifest};
use crate::quarantine::reconcile::reconcile;
use crate::quarantine::report::diagnostic;

/// Error code for a session whose `manifest.json` cannot be understood.
pub const MANIFEST_CORRUPT: &str = "manifest_corrupt";
/// Error code for a stored item no manifest entry accounts for.
pub const ORPHANED_ITEM: &str = "orphaned_item";

/// One session of the store, reconciled with what is on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSession {
    /// Identifier of the session.
    pub id: SessionId,
    /// Directory holding it, always a direct child of the store root.
    pub dir: PathBuf,
    /// Its `manifest.json`, with every `moving` entry settled.
    pub manifest: Manifest,
    /// Stored items no entry accounts for.
    pub orphans: Vec<PathBuf>,
}

impl StoredSession {
    /// The session the manifest describes.
    pub fn session(&self) -> &QuarantineSession {
        &self.manifest.session
    }

    /// `true` when everything under `items/` is accounted for by an entry.
    ///
    /// A session that is not may still be purged on purpose, but no automatic
    /// step removes it: the orphans are data Broza moved and lost track of.
    pub fn is_accounted_for(&self) -> bool {
        self.orphans.is_empty()
    }

    /// One `errors[]` entry per orphan.
    pub fn orphan_errors(&self) -> Vec<ErrorEntry> {
        self.orphans
            .iter()
            .map(|stored| {
                diagnostic(
                    ORPHANED_ITEM,
                    format!(
                        "quarantine session `{}` holds `{}`, which its manifest does not list",
                        self.id,
                        stored.display()
                    ),
                    Some(stored),
                )
            })
            .collect()
    }
}

/// What a read of the whole store found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreContents {
    /// The sessions that could be read, newest first.
    pub sessions: Vec<StoredSession>,
    /// One entry per session that could not be; a non-empty list means exit `5`.
    pub errors: Vec<ErrorEntry>,
}

/// Every session in the store, newest first.
///
/// A store that does not exist yet holds no sessions; a child that is not named
/// like a session, or is not a directory, is not one and is ignored. So is a
/// session directory with no manifest at all: the mover writes the manifest
/// before it moves anything, so such a directory provably holds no items.
///
/// A session whose manifest is corrupt is **skipped and reported**, never
/// allowed to hide the rest of the store.
///
/// # Errors
///
/// Only what reading the root itself reports.
pub fn read_all(fs: &dyn FileOps, root: &Path) -> Result<StoreContents, BrozaError> {
    if !fs.exists(root) {
        return Ok(StoreContents::default());
    }
    let mut found = StoreContents::default();
    for child in fs.read_dir(root)? {
        let Some(id) = session_id_of(&child) else {
            continue;
        };
        if !fs.metadata(&child)?.is_dir {
            continue;
        }
        match read_one(fs, root, &id) {
            Ok(session) => found.sessions.push(session),
            Err(BrozaError::TargetNotFound(_)) => {}
            Err(error) => found.errors.push(corrupt(&child, &error)),
        }
    }
    found.sessions.sort_by(|left, right| {
        right.session().created_at.cmp(&left.session().created_at).then_with(|| right.id.cmp(&left.id))
    });
    Ok(found)
}

/// The identifiers of every readable session in the store, newest first.
///
/// # Errors
///
/// See [`read_all`].
pub fn all_ids(fs: &dyn FileOps, root: &Path) -> Result<Vec<SessionId>, BrozaError> {
    Ok(read_all(fs, root)?.sessions.into_iter().map(|stored| stored.id).collect())
}

/// One session of the store, by identifier, reconciled with the disk.
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
    let settled = reconcile(&manifest.session, &dir, fs)?;
    Ok(StoredSession {
        id: id.clone(),
        dir,
        manifest: manifest.with_session(settled.session),
        orphans: settled.orphans,
    })
}

/// The `errors[]` entry a session that cannot be read produces.
pub fn corrupt(dir: &Path, error: &BrozaError) -> ErrorEntry {
    diagnostic(
        MANIFEST_CORRUPT,
        format!("quarantine session `{}` cannot be read: {error}", dir.display()),
        Some(dir),
    )
}

/// The identifier a session directory is named after, if it is one.
fn session_id_of(path: &Path) -> Option<SessionId> {
    path.file_name()?.to_str()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{MANIFEST_CORRUPT, all_ids, read_all, read_one};
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

    fn count(fs: &FakeFileOps) -> usize {
        read_all(fs, Path::new(ROOT)).map(|found| found.sessions.len()).unwrap_or_default()
    }

    #[test]
    fn a_store_that_was_never_used_holds_no_sessions() {
        let fs = FakeFileOps::new().with_root("/", 1);

        assert_eq!(read_all(&fs, Path::new(ROOT)).ok(), Some(super::StoreContents::default()));
    }

    #[test]
    fn an_empty_store_holds_no_sessions() {
        assert_eq!(count(&store_fs()), 0);
    }

    #[test]
    fn a_session_is_found_by_the_name_of_its_directory() {
        let fs = session_fs();
        with_session(&fs);

        let found = read_all(&fs, Path::new(ROOT)).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(found.sessions.len(), 1);
        assert_eq!(found.sessions[0].session().state, SessionState::Complete);
        assert!(found.errors.is_empty());
        assert_eq!(all_ids(&fs, Path::new(ROOT)).ok(), Some(vec![found.sessions[0].id.clone()]));
    }

    #[test]
    fn a_directory_that_is_not_named_like_a_session_is_ignored() {
        let fs = session_fs();
        with_session(&fs);
        fs.add_dir(format!("{ROOT}/scratch"));
        fs.add_file(format!("{ROOT}/README"), b"not a session");

        assert_eq!(count(&fs), 1);
    }

    #[test]
    fn a_file_named_like_a_session_is_not_one() {
        let fs = store_fs();
        fs.add_file(format!("{ROOT}/cln_20260801091200_c3d4"), b"impostor");

        assert_eq!(count(&fs), 0);
    }

    #[test]
    fn a_session_directory_without_a_manifest_holds_nothing_and_is_ignored() {
        assert_eq!(count(&session_fs()), 0);
    }

    #[test]
    fn a_corrupt_manifest_is_reported_and_never_hides_the_rest_of_the_store() {
        let fs = session_fs();
        with_session(&fs);
        let healthy = format!("{ROOT}/cln_20260921103608_c3d4");
        fs.add_dir(&healthy);
        let broken = format!("{ROOT}/cln_20260722081500_e5f6/manifest.json");
        fs.add_file(&broken, b"{");
        let raw = String::from_utf8(fs.read(&manifest_file()).unwrap_or_default()).unwrap_or_default();
        fs.add_file(format!("{healthy}/manifest.json"), raw.replace("_a1b2", "_c3d4").as_bytes());

        let found = read_all(&fs, Path::new(ROOT)).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(found.sessions.len(), 2, "the readable sessions are still listed");
        assert_eq!(found.errors.len(), 1);
        assert_eq!(found.errors[0].code, MANIFEST_CORRUPT);
        assert!(found.errors[0].message.contains("cln_20260722081500_e5f6"), "{:?}", found.errors[0]);
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

        let found = read_all(&fs, Path::new(ROOT)).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(found.sessions.len(), 0);
        assert_eq!(found.errors.len(), 1);
    }

    #[test]
    fn a_session_holding_an_item_nobody_listed_reports_it() {
        let fs = session_fs();
        with_session(&fs);
        fs.add_file(crate::quarantine::fixtures::session_dir().join("items/0009/mystery"), b"x");
        let id = crate::quarantine::fixtures::session_id();

        let found = read_one(&fs, Path::new(ROOT), &id).unwrap_or_else(|error| panic!("{error}"));

        assert!(!found.is_accounted_for());
        assert_eq!(found.orphan_errors().len(), 1);
        assert_eq!(found.orphan_errors()[0].code, super::ORPHANED_ITEM);
    }
}
