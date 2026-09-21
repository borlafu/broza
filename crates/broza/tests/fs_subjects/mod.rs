//! The two [`FileOps`] implementations under contract test, over one shared tree.
//!
//! Used by `fakes_behave_like_std.rs` and `fakes_behave_like_std_posix.rs`. Each
//! test binary compiles this module separately and uses part of it, so unused
//! helpers are expected here.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use broza::BrozaError;
use broza::adapters::StdFileOps;
use broza::ports::{EntryMetadata, FileOps};
use broza::testing::FakeFileOps;
use tempfile::TempDir;

/// Root of the in-memory tree.
pub const FAKE_ROOT: &str = "/fake";
/// Device of the in-memory root.
pub const FAKE_DEVICE: u64 = 1;
/// Second in-memory root, on another device.
pub const FAKE_OTHER_ROOT: &str = "/other";
/// Device of the second in-memory root.
pub const FAKE_OTHER_DEVICE: u64 = 2;
/// Contents of the file every test starts from.
pub const FILE_CONTENTS: &[u8] = b"hello";
/// Contents of the second file.
pub const OTHER_CONTENTS: &[u8] = b"other";
/// The file inside `dir`, relative to the root.
pub const FILE: &str = "dir/file.txt";
/// A symlink to [`FILE`], relative to the root.
pub const FILE_LINK: &str = "link";
/// A symlink to the `dir` directory, relative to the root.
pub const DIR_LINK: &str = "dirlink";

/// `ENOENT`, a missing path.
pub const ENOENT: i32 = 2;
/// `EEXIST`, a path that is already taken.
pub const EEXIST: i32 = 17;
/// `ENOTDIR`, a non-directory used as a directory.
pub const ENOTDIR: i32 = 20;
/// `EISDIR`, a directory used as a file.
pub const EISDIR: i32 = 21;
/// `ENOTEMPTY`, a directory that still has children.
pub const ENOTEMPTY: i32 = 66;
/// `EXDEV`, a rename that crosses devices.
pub const EXDEV: i32 = 18;

/// Creates a symlink at the first path pointing at the second.
type SymlinkFn = Box<dyn Fn(&Path, &Path)>;

/// One implementation under test, rooted in its own scratch tree.
pub struct Subject {
    /// Name reported when an assertion fails.
    pub name: &'static str,
    /// Implementation under test.
    pub fs: Arc<dyn FileOps>,
    /// Directory every path in the test is relative to.
    pub root: PathBuf,
    /// Creates a symlink, which [`FileOps`] deliberately cannot do.
    symlink: SymlinkFn,
    /// Kept alive so the temporary directory outlives the test.
    _tempdir: Option<TempDir>,
}

impl Subject {
    /// Absolute path of `relative` inside this subject's root.
    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// Metadata of `relative`, failing the test when it cannot be read.
    pub fn metadata(&self, relative: &str) -> EntryMetadata {
        self.fs
            .metadata(&self.path(relative))
            .unwrap_or_else(|e| panic!("{}: metadata {relative}: {e}", self.name))
    }

    /// Create a symlink at `relative` pointing at `target`.
    pub fn symlink(&self, relative: &str, target: &str) {
        (self.symlink)(&self.path(relative), Path::new(target));
    }

    /// Fail the test unless `result` is the errno the real filesystem returns.
    pub fn expect_errno<T: std::fmt::Debug>(&self, what: &str, result: Result<T, BrozaError>, errno: i32) {
        match result {
            Ok(value) => panic!("{}: {what} unexpectedly succeeded with {value:?}", self.name),
            Err(BrozaError::Io { source, .. }) => {
                assert_eq!(source.raw_os_error(), Some(errno), "{}: {what}", self.name);
            }
            Err(other) => panic!("{}: {what} failed with {other:?}, expected errno {errno}", self.name),
        }
    }

    /// Fail the test unless `result` reports the target as not found.
    pub fn expect_not_found<T: std::fmt::Debug>(&self, what: &str, result: &Result<T, BrozaError>) {
        assert!(
            matches!(result, Err(BrozaError::TargetNotFound(_))),
            "{}: {what} gave {result:?}",
            self.name
        );
    }

    /// Contents of `relative`, failing the test when it cannot be read.
    pub fn read(&self, relative: &str) -> Vec<u8> {
        self.fs.read(&self.path(relative)).unwrap_or_else(|e| panic!("{}: read {relative}: {e}", self.name))
    }

    /// Sorted names of the direct children of `relative`.
    pub fn child_names(&self, relative: &str) -> Vec<String> {
        let mut names = self
            .children(relative)
            .iter()
            .filter_map(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    /// Direct children of `relative`, as the implementation returns them.
    pub fn children(&self, relative: &str) -> Vec<PathBuf> {
        self.fs
            .read_dir(&self.path(relative))
            .unwrap_or_else(|e| panic!("{}: read_dir {relative}: {e}", self.name))
    }
}

/// The in-memory subject: two roots on different devices.
pub fn fake_subject() -> Subject {
    let fake = Arc::new(
        FakeFileOps::new().with_root(FAKE_ROOT, FAKE_DEVICE).with_root(FAKE_OTHER_ROOT, FAKE_OTHER_DEVICE),
    );
    let for_symlink = Arc::clone(&fake);
    Subject {
        name: "FakeFileOps",
        fs: Arc::clone(&fake) as Arc<dyn FileOps>,
        root: PathBuf::from(FAKE_ROOT),
        symlink: Box::new(move |path, target| for_symlink.add_symlink(path, target)),
        _tempdir: None,
    }
}

/// The real subject: a temporary directory on the real filesystem.
pub fn std_subject() -> Subject {
    let tempdir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let root = tempdir.path().to_path_buf();
    Subject {
        name: "StdFileOps",
        fs: Arc::new(StdFileOps),
        root,
        symlink: Box::new(|path, target| {
            std::os::unix::fs::symlink(target, path)
                .unwrap_or_else(|e| panic!("symlink {}: {e}", path.display()));
        }),
        _tempdir: Some(tempdir),
    }
}

/// Both subjects, each populated with the same tree:
///
/// ```text
/// dir/file.txt   "hello"        empty/          an empty directory
/// other.txt      "other"        full/keep.txt   a directory with a child
/// link    -> dir/file.txt       dirlink -> dir
/// ```
pub fn subjects() -> Vec<Subject> {
    [fake_subject(), std_subject()].into_iter().map(populate).collect()
}

/// Build the shared tree inside `subject`.
fn populate(subject: Subject) -> Subject {
    for directory in ["dir", "empty", "full"] {
        subject
            .fs
            .create_dir_all(&subject.path(directory))
            .unwrap_or_else(|e| panic!("{}: create_dir_all {directory}: {e}", subject.name));
    }
    let files: [(&str, &[u8]); 3] =
        [(FILE, FILE_CONTENTS), ("other.txt", OTHER_CONTENTS), ("full/keep.txt", b"keep")];
    for (relative, contents) in files {
        subject
            .fs
            .write_atomic(&subject.path(relative), contents)
            .unwrap_or_else(|e| panic!("{}: write_atomic {relative}: {e}", subject.name));
    }
    subject.symlink(FILE_LINK, FILE);
    subject.symlink(DIR_LINK, "dir");
    subject
}
