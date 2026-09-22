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
/// Directory holding what only a real filesystem can express.
///
/// It exists on both subjects and is empty on the in-memory one: a FIFO and a
/// resource fork have no meaning there.
pub const SPECIAL_DIR: &str = "special";
/// A file carrying a resource fork, relative to the root. Real subject only.
///
/// `lstat` reports the data fork in `st_size` but counts both forks in
/// `st_blocks`; a reader that asks the kernel for the wrong pair of attributes
/// disagrees with it here and nowhere else.
pub const FORKED_FILE: &str = "special/forked.txt";
/// Contents of the data fork of [`FORKED_FILE`].
pub const FORKED_CONTENTS: &[u8] = b"data fork";
/// Bytes written into the resource fork of [`FORKED_FILE`].
pub const RESOURCE_FORK_BYTES: usize = 77;
/// A FIFO, relative to the root. Real subject only.
pub const FIFO: &str = "special/pipe";
/// An empty directory, relative to the root.
pub const EMPTY_DIR: &str = "empty";

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

/// Creates a hard link at the second path for the entry at the first.
type HardLinkFn = Box<dyn Fn(&Path, &Path)>;

/// Adds the shapes only a real filesystem has: a FIFO, a resource fork.
type SpecialsFn = Box<dyn Fn(&Path)>;

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
    /// Creates a hard link, which [`FileOps`] deliberately cannot do.
    hard_link: HardLinkFn,
    /// Fills [`SPECIAL_DIR`]; does nothing on the in-memory subject.
    specials: SpecialsFn,
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

    /// Give the entry at `relative` a second name at `link`.
    pub fn hard_link(&self, relative: &str, link: &str) {
        (self.hard_link)(&self.path(relative), &self.path(link));
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

    /// `len` bytes of `relative` from `offset`, or the test fails.
    pub fn read_range(&self, relative: &str, offset: u64, len: usize) -> Vec<u8> {
        self.fs
            .read_range(&self.path(relative), offset, len)
            .unwrap_or_else(|e| panic!("{}: read_range {relative} {offset} {len}: {e}", self.name))
    }

    /// The content hash of `relative`, or the test fails.
    pub fn hash_file(&self, relative: &str) -> broza::ports::ContentHash {
        self.fs
            .hash_file(&self.path(relative))
            .unwrap_or_else(|e| panic!("{}: hash {relative}: {e}", self.name))
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
    let for_link = Arc::clone(&fake);
    Subject {
        name: "FakeFileOps",
        fs: Arc::clone(&fake) as Arc<dyn FileOps>,
        root: PathBuf::from(FAKE_ROOT),
        symlink: Box::new(move |path, target| for_symlink.add_symlink(path, target)),
        hard_link: Box::new(move |existing, link| for_link.add_hard_link(existing, link)),
        // A FIFO and a resource fork are filesystem shapes, not tree shapes:
        // the in-memory subject has nowhere to put them.
        specials: Box::new(|_root| {}),
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
        hard_link: Box::new(|existing, link| {
            std::fs::hard_link(existing, link)
                .unwrap_or_else(|e| panic!("hard link {}: {e}", link.display()));
        }),
        specials: Box::new(|root| {
            let forked = root.join(FORKED_FILE);
            std::fs::write(&forked, FORKED_CONTENTS)
                .unwrap_or_else(|e| panic!("write {}: {e}", forked.display()));
            // The resource fork is reached through the file's own directory.
            std::fs::write(forked.join("..namedfork/rsrc"), vec![7_u8; RESOURCE_FORK_BYTES])
                .unwrap_or_else(|e| panic!("resource fork {}: {e}", forked.display()));
            // `mkfifo(1)` rather than `libc::mkfifo`: an integration test is
            // outside `adapters/`, where `unsafe` is denied.
            let made = std::process::Command::new("mkfifo")
                .arg(root.join(FIFO))
                .status()
                .unwrap_or_else(|e| panic!("mkfifo: {e}"));
            assert!(made.success(), "mkfifo failed: {made}");
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
/// Build the shared tree inside one subject, for a test that wants only one.
pub fn populate_for_test(subject: Subject) -> Subject {
    populate(subject)
}

fn populate(subject: Subject) -> Subject {
    for directory in ["dir", "empty", "full", SPECIAL_DIR] {
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
    (subject.specials)(&subject.root);
    subject
}
