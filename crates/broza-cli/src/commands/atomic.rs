//! Atomic file writes: unpredictable temp file in the destination directory,
//! `fsync`, rename, `fsync` of the directory.
//!
//! The same shape the quarantine manifest uses (`docs/cli-spec.md` §3.5): a
//! reader never observes a half-written file, and a crash between the two steps
//! leaves either the old file or the new one, never a truncated one.
//!
//! The temporary file is created with [`tempfile::NamedTempFile::new_in`], so
//! its name is unpredictable and a symlink sitting at the destination is never
//! followed — the rename replaces the symlink itself.

use std::io::Write;
use std::path::{Path, PathBuf};

use broza::BrozaError;
use tempfile::NamedTempFile;

/// Write `contents` to `path`, creating parent directories as needed.
///
/// # Errors
///
/// [`BrozaError::PermissionDenied`] on `EACCES`/`EPERM` (exit `3`) and
/// [`BrozaError::Io`] for any other failure (exit `1`); both name the path.
pub fn write(path: &Path, contents: &str) -> Result<(), BrozaError> {
    let parent = parent_of(path);
    std::fs::create_dir_all(&parent)
        .map_err(|source| BrozaError::from_io("cannot create", &parent, source))?;

    let mut temp = NamedTempFile::new_in(&parent)
        .map_err(|source| BrozaError::from_io("cannot create a temporary file in", &parent, source))?;
    temp.write_all(contents.as_bytes())
        .map_err(|source| BrozaError::from_io("cannot write", path, source))?;
    temp.as_file().sync_all().map_err(|source| BrozaError::from_io("cannot flush", path, source))?;
    temp.persist(path).map_err(|error| BrozaError::from_io("cannot replace", path, error.error))?;

    sync_directory(&parent)
}

/// Directory the file lives in; an empty parent means the current directory.
fn parent_of(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Persist the rename itself, not just the file contents.
fn sync_directory(dir: &Path) -> Result<(), BrozaError> {
    let handle =
        std::fs::File::open(dir).map_err(|source| BrozaError::from_io("cannot open", dir, source))?;
    handle.sync_all().map_err(|source| BrozaError::from_io("cannot flush", dir, source))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn temp_files_in(dir: &Path) -> usize {
        std::fs::read_dir(dir).map(|entries| entries.filter_map(Result::ok).count()).unwrap_or_default()
    }

    #[test]
    fn writes_the_contents_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write(&path, "min-size = \"2GB\"\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "min-size = \"2GB\"\n");
        assert_eq!(temp_files_in(dir.path()), 1, "only the destination must remain");
    }

    #[test]
    fn replaces_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write(&path, "one").unwrap();
        write(&path, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        assert_eq!(temp_files_in(dir.path()), 1);
    }

    #[test]
    fn creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c/config.toml");
        write(&path, "x").unwrap();
        assert!(path.exists());
    }

    #[test]
    fn a_symlink_at_the_destination_is_replaced_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.toml");
        std::fs::write(&outside, "original").unwrap();
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        write(&link, "new").unwrap();

        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "original", "target untouched");
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "new");
        assert!(!std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    }

    #[test]
    fn reports_the_path_when_the_directory_cannot_be_created() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "not a directory").unwrap();
        let err = write(&blocker.join("config.toml"), "x").expect_err("must fail");
        assert!(err.to_string().contains("blocker"), "{err}");
        assert_eq!(broza::ExitCode::from(&err), broza::ExitCode::GenericError);
    }

    #[test]
    fn a_read_only_directory_is_a_permission_error() {
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        set_mode(&locked, 0o500);

        let err = write(&locked.join("config.toml"), "x").expect_err("must fail");
        set_mode(&locked, 0o700);

        assert!(matches!(err, BrozaError::PermissionDenied { .. }), "{err}");
        assert_eq!(broza::ExitCode::from(&err), broza::ExitCode::PermissionDenied);
    }

    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
}
