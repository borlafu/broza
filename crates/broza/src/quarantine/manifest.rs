//! The on-disk `manifest.json` of one quarantine session.
//!
//! The file wraps the [`QuarantineSession`] of `docs/cli-spec.md` §4.5 in a
//! versioned envelope, so a future layout change is detectable instead of being
//! mistaken for corruption:
//!
//! ```json
//! { "manifest_version": 1, "session": { "id": "cln_...", "entries": [] } }
//! ```
//!
//! Reading never panics and never rewrites what it does not understand: unknown
//! `status`, `state` and `error` tokens survive verbatim through the model's open
//! enums, and a file that is not valid JSON becomes an error naming the path.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::BrozaError;
use crate::model::QuarantineSession;
use crate::ports::FileOps;

/// Layout version written by this Broza.
pub const MANIFEST_VERSION: u32 = 1;

/// The content of a session's `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Manifest {
    /// Version of the manifest layout; [`MANIFEST_VERSION`] for this Broza.
    pub manifest_version: u32,
    /// The session, with its entries.
    pub session: QuarantineSession,
}

impl Manifest {
    /// Wrap `session` in a manifest of the current version.
    pub fn new(session: QuarantineSession) -> Self {
        Self { manifest_version: MANIFEST_VERSION, session }
    }

    /// Return a copy carrying `session` instead.
    #[must_use]
    pub fn with_session(self, session: QuarantineSession) -> Self {
        Self { session, ..self }
    }
}

/// Read the manifest at `path`.
///
/// # Errors
///
/// [`BrozaError::TargetNotFound`] when there is no manifest (the session
/// directory is empty or was removed), [`BrozaError::PermissionDenied`] when it
/// cannot be read, and [`BrozaError::Other`] naming the path when the bytes are
/// not a manifest this Broza understands — a corrupt store is reported, never
/// fixed up and never a panic.
pub fn read(fs: &dyn FileOps, path: &Path) -> Result<Manifest, BrozaError> {
    let bytes = fs.read(path)?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|error| corrupt(path, &format!("invalid JSON: {error}")))?;
    if manifest.manifest_version != MANIFEST_VERSION {
        return Err(corrupt(
            path,
            &format!(
                "manifest version {} is not supported, this Broza writes version {MANIFEST_VERSION}",
                manifest.manifest_version
            ),
        ));
    }
    manifest.session.validate_manifest().map_err(|error| corrupt(path, &error.to_string()))?;
    Ok(manifest)
}

/// Write `manifest` to `path` atomically (temp file + rename).
///
/// # Errors
///
/// [`BrozaError::Other`] when the session is not valid manifest content (see
/// [`QuarantineSession::validate_manifest`]), and whatever
/// [`FileOps::write_atomic`] reports otherwise.
pub fn write(fs: &dyn FileOps, path: &Path, manifest: &Manifest) -> Result<(), BrozaError> {
    manifest.session.validate_manifest()?;
    let mut bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| BrozaError::Other(format!("quarantine manifest `{}`: {error}", path.display())))?;
    bytes.push(b'\n');
    fs.write_atomic(path, &bytes)
}

/// The error a manifest that cannot be trusted produces.
fn corrupt(path: &Path, reason: &str) -> BrozaError {
    BrozaError::Other(format!("quarantine manifest `{}` is corrupt: {reason}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{MANIFEST_VERSION, Manifest, read, write};
    use crate::BrozaError;
    use crate::model::{ItemStatus, SessionState};
    use crate::ports::FileOps;
    use crate::quarantine::fixtures::{entry, manifest_file, session, session_fs};
    use crate::testing::FakeFileOps;

    fn path() -> PathBuf {
        manifest_file()
    }

    fn written(fs: &FakeFileOps, state: SessionState) -> Manifest {
        let entries = vec![entry(1, "/Users/dana/Library/Caches/app", 10, ItemStatus::Quarantined)];
        let manifest = Manifest::new(session(state, entries));
        write(fs, &path(), &manifest).unwrap_or_else(|error| panic!("{error}"));
        manifest
    }

    fn text(fs: &FakeFileOps) -> String {
        String::from_utf8(fs.read(&path()).unwrap_or_else(|error| panic!("{error}"))).unwrap_or_default()
    }

    #[test]
    fn a_manifest_round_trips_through_the_filesystem() {
        let fs = session_fs();

        let manifest = written(&fs, SessionState::Complete);

        assert_eq!(read(&fs, &path()).ok(), Some(manifest));
        assert!(text(&fs).contains("\"manifest_version\": 1"), "{}", text(&fs));
        assert!(text(&fs).ends_with('\n'), "a text file ends with a newline");
    }

    #[test]
    fn a_missing_manifest_is_a_missing_target_not_a_panic() {
        let fs = session_fs();

        let error = read(&fs, &path());

        assert!(matches!(error, Err(BrozaError::TargetNotFound(_))), "{error:?}");
    }

    #[test]
    fn a_corrupt_manifest_is_reported_with_its_path() {
        let fs = session_fs();
        fs.add_file(path(), b"{ not json");

        let error = read(&fs, &path());

        let Err(BrozaError::Other(message)) = error else { panic!("{error:?}") };
        assert!(message.contains(&path().display().to_string()), "{message}");
        assert!(message.contains("corrupt"), "{message}");
    }

    #[test]
    fn a_manifest_from_a_newer_layout_is_refused_instead_of_guessed() {
        let fs = session_fs();
        written(&fs, SessionState::Complete);
        let raw = text(&fs).replace("\"manifest_version\": 1", "\"manifest_version\": 2");
        fs.add_file(path(), raw.as_bytes());

        let error = read(&fs, &path());

        let Err(BrozaError::Other(message)) = error else { panic!("{error:?}") };
        assert!(message.contains("version 2"), "{message}");
    }

    #[test]
    fn a_state_token_from_a_newer_broza_survives_the_round_trip() {
        let fs = session_fs();
        written(&fs, SessionState::Complete);
        let raw = text(&fs).replace("\"complete\"", "\"archived\"");
        fs.add_file(path(), raw.as_bytes());

        let manifest = read(&fs, &path()).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(manifest.session.state, SessionState::Unknown("archived".to_owned()));
        write(&fs, &path(), &manifest).unwrap_or_else(|error| panic!("{error}"));
        assert!(text(&fs).contains("\"archived\""), "{}", text(&fs));
    }

    #[test]
    fn a_manifest_that_stores_the_derived_expired_state_is_refused_both_ways() {
        let fs = session_fs();
        let manifest = Manifest::new(session(SessionState::Expired, Vec::new()));

        assert!(write(&fs, &path(), &manifest).is_err(), "expired is derived, never stored");

        written(&fs, SessionState::Complete);
        let raw = text(&fs).replace("\"complete\"", "\"expired\"");
        fs.add_file(path(), raw.as_bytes());
        assert!(matches!(read(&fs, &path()), Err(BrozaError::Other(_))));
    }

    #[test]
    fn replacing_the_session_keeps_the_layout_version() {
        let manifest = Manifest::new(session(SessionState::InProgress, Vec::new()));

        let updated = manifest.with_session(session(SessionState::Complete, Vec::new()));

        assert_eq!(updated.manifest_version, MANIFEST_VERSION);
        assert_eq!(updated.session.state, SessionState::Complete);
    }
}
