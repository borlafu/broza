//! Atomic file writes: temp file in the destination directory, then rename.
//!
//! The same shape the quarantine manifest uses (`docs/cli-spec.md` §3.5): a
//! reader never observes a half-written file.

use std::path::{Path, PathBuf};

use broza::BrozaError;

/// Suffix of the temporary file written next to the destination.
const TEMP_SUFFIX: &str = ".tmp";

/// Write `contents` to `path`, creating parent directories as needed.
///
/// # Errors
///
/// [`BrozaError::Config`] when the directory, the temporary file or the rename
/// fails; the message always names the path.
pub fn write(path: &Path, contents: &str) -> Result<(), BrozaError> {
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|source| failure(parent, "create the directory", &source))?;
    }
    let temp = temp_path(path);
    std::fs::write(&temp, contents).map_err(|source| failure(&temp, "write", &source))?;
    std::fs::rename(&temp, path).map_err(|source| {
        let _ignored = std::fs::remove_file(&temp);
        failure(path, "replace", &source)
    })
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(TEMP_SUFFIX);
    PathBuf::from(name)
}

fn failure(path: &Path, action: &str, source: &std::io::Error) -> BrozaError {
    BrozaError::Config(format!("cannot {action} {}: {source}", path.display()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn writes_the_contents_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write(&path, "min-size = \"2GB\"\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "min-size = \"2GB\"\n");
        assert!(!temp_path(&path).exists());
    }

    #[test]
    fn replaces_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write(&path, "one").unwrap();
        write(&path, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
    }

    #[test]
    fn creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c/config.toml");
        write(&path, "x").unwrap();
        assert!(path.exists());
    }

    #[test]
    fn reports_the_path_when_the_directory_cannot_be_created() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "not a directory").unwrap();
        let err = write(&blocker.join("config.toml"), "x").expect_err("must fail");
        assert!(err.to_string().contains("blocker"), "{err}");
    }
}
