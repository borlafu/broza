//! Real filesystem adapter built on `std::fs` and `std::os::unix::fs::MetadataExt`.
//!
//! No `libc` and no `unsafe`: everything Broza needs from `stat` is already exposed by
//! [`MetadataExt`]. Symlinks are never followed — `metadata` is an `lstat` and
//! `remove_tree` on a link removes the link, not what it points at.

use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::BrozaError;
use crate::adapters::io_error::from_io;
use crate::ports::{EntryMetadata, FileOps};

/// Size of the blocks `st_blocks` counts, fixed at 512 bytes by POSIX.
const STAT_BLOCK_BYTES: u64 = 512;

/// [`FileOps`] backed by the real filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdFileOps;

impl FileOps for StdFileOps {
    fn metadata(&self, path: &Path) -> Result<EntryMetadata, BrozaError> {
        let meta = fs::symlink_metadata(path)
            .map_err(|source| from_io(format!("stat {}", path.display()), path, source))?;
        let kind = meta.file_type();
        Ok(EntryMetadata {
            device: meta.dev(),
            inode: meta.ino(),
            size_bytes: meta.size(),
            allocated_bytes: meta.blocks().saturating_mul(STAT_BLOCK_BYTES),
            link_count: meta.nlink(),
            is_dir: kind.is_dir(),
            is_symlink: kind.is_symlink(),
            modified: timestamp(meta.mtime(), meta.mtime_nsec()),
            accessed: timestamp(meta.atime(), meta.atime_nsec()),
        })
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>, BrozaError> {
        let entries = fs::read_dir(path)
            .map_err(|source| from_io(format!("read directory {}", path.display()), path, source))?;
        let mut children = Vec::new();
        for entry in entries {
            let entry = entry
                .map_err(|source| from_io(format!("read directory {}", path.display()), path, source))?;
            children.push(entry.path());
        }
        Ok(children)
    }

    fn exists(&self, path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok()
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), BrozaError> {
        let context = format!("rename {} to {}", from.display(), to.display());
        fs::rename(from, to).map_err(|source| from_io(context, from, source))
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), BrozaError> {
        fs::create_dir_all(path)
            .map_err(|source| from_io(format!("create directory {}", path.display()), path, source))
    }

    fn remove_tree(&self, path: &Path) -> Result<(), BrozaError> {
        let context = format!("remove {}", path.display());
        let meta =
            fs::symlink_metadata(path).map_err(|source| from_io(context.clone(), path, source))?.file_type();
        let removed = if meta.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) };
        removed.map_err(|source| from_io(context, path, source))
    }

    fn write_atomic(&self, path: &Path, contents: &[u8]) -> Result<(), BrozaError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let context = format!("write {}", path.display());
        let mut temp = tempfile::NamedTempFile::new_in(parent)
            .map_err(|source| from_io(context.clone(), parent, source))?;
        temp.write_all(contents).map_err(|source| from_io(context.clone(), path, source))?;
        temp.as_file().sync_all().map_err(|source| from_io(context.clone(), path, source))?;
        temp.persist(path).map_err(|error| from_io(context, path, error.error))?;
        sync_directory(parent)
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>, BrozaError> {
        fs::read(path).map_err(|source| from_io(format!("read {}", path.display()), path, source))
    }
}

/// Flush the directory entry itself so the rename survives a crash.
fn sync_directory(path: &Path) -> Result<(), BrozaError> {
    let context = format!("sync directory {}", path.display());
    let dir = fs::File::open(path).map_err(|source| from_io(context.clone(), path, source))?;
    dir.sync_all().map_err(|source| from_io(context, path, source))
}

/// Convert a `stat` time into a [`Timestamp`], dropping values outside its range.
fn timestamp(seconds: i64, nanoseconds: i64) -> Option<Timestamp> {
    let nanoseconds = i32::try_from(nanoseconds).ok()?;
    Timestamp::new(seconds, nanoseconds).ok()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{STAT_BLOCK_BYTES, StdFileOps, timestamp};
    use crate::BrozaError;
    use crate::ports::FileOps;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"))
    }

    #[test]
    fn allocated_size_counts_whole_blocks() {
        let dir = tempdir();
        let path = dir.path().join("file");
        StdFileOps.write_atomic(&path, &[0_u8; 100]).unwrap_or_else(|e| panic!("{e}"));

        let meta = StdFileOps.metadata(&path).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(meta.size_bytes, 100);
        assert!(meta.allocated_bytes >= STAT_BLOCK_BYTES, "{}", meta.allocated_bytes);
        assert_eq!(meta.allocated_bytes % STAT_BLOCK_BYTES, 0);
    }

    #[test]
    fn writing_into_a_missing_directory_reports_the_directory_as_not_found() {
        let dir = tempdir();
        let path = dir.path().join("missing/file");

        let err = StdFileOps.write_atomic(&path, b"x").err();

        assert!(matches!(err, Some(BrozaError::TargetNotFound(_))), "{err:?}");
    }

    #[test]
    fn writing_atomically_leaves_no_temporary_file_behind() {
        let dir = tempdir();
        let path = dir.path().join("file");

        StdFileOps.write_atomic(&path, b"x").unwrap_or_else(|e| panic!("{e}"));

        let children = StdFileOps.read_dir(dir.path()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(children, vec![path]);
    }

    #[test]
    fn creating_a_directory_twice_succeeds() {
        let dir = tempdir();
        let path = dir.path().join("a/b/c");

        StdFileOps.create_dir_all(&path).unwrap_or_else(|e| panic!("{e}"));
        StdFileOps.create_dir_all(&path).unwrap_or_else(|e| panic!("{e}"));

        assert!(StdFileOps.exists(&path));
    }

    #[test]
    fn a_directory_cannot_be_created_where_a_file_already_is() {
        let dir = tempdir();
        let path = dir.path().join("file");
        StdFileOps.write_atomic(&path, b"x").unwrap_or_else(|e| panic!("{e}"));

        let err = StdFileOps.create_dir_all(&path.join("child")).err();

        assert!(matches!(err, Some(BrozaError::Io { .. })), "{err:?}");
    }

    #[test]
    fn writing_over_a_directory_fails_and_leaves_the_directory_alone() {
        let dir = tempdir();
        let path = dir.path().join("subdir");
        StdFileOps.create_dir_all(&path).unwrap_or_else(|e| panic!("{e}"));

        let err = StdFileOps.write_atomic(&path, b"x").err();

        assert!(err.is_some(), "writing over a directory must fail");
        assert!(StdFileOps.metadata(&path).is_ok_and(|meta| meta.is_dir));
    }

    #[test]
    fn renaming_a_missing_entry_reports_the_source_as_not_found() {
        let dir = tempdir();

        let err = StdFileOps.rename(&dir.path().join("a"), &dir.path().join("b")).err();

        assert!(matches!(err, Some(BrozaError::TargetNotFound(_))), "{err:?}");
    }

    #[test]
    fn a_stat_time_out_of_range_is_reported_as_unknown() {
        assert!(timestamp(0, 0).is_some());
        assert!(timestamp(i64::MAX, 0).is_none());
        assert!(timestamp(0, i64::from(i32::MAX) + 1).is_none());
    }

    #[test]
    fn reading_a_directory_as_a_file_fails_without_panicking() {
        let dir = tempdir();

        let err = StdFileOps.read(dir.path()).err();

        assert!(err.is_some());
        assert!(!StdFileOps.exists(Path::new("/definitely/not/here")));
    }
}
