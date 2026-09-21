//! Contract tests: [`FakeFileOps`] must be indistinguishable from [`StdFileOps`].
//!
//! Every test runs against both implementations over the same synthetic tree. A fake
//! that drifts from the real filesystem turns unit tests into wishful thinking, so the
//! two are pinned together here (`docs/implementation-plan.md` §3.6).
//!
//! Only the `test-support` feature exposes `broza::testing`; without it this file
//! compiles to nothing.
#![cfg(feature = "test-support")]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use broza::BrozaError;
use broza::adapters::StdFileOps;
use broza::ports::{EntryMetadata, FileOps};
use broza::testing::FakeFileOps;
use tempfile::TempDir;

/// Root of the in-memory tree.
const FAKE_ROOT: &str = "/fake";
/// Device of the in-memory root.
const FAKE_DEVICE: u64 = 1;
/// Device of a second in-memory root, used for the cross-device rename.
const FAKE_OTHER_DEVICE: u64 = 2;
/// Second in-memory root.
const FAKE_OTHER_ROOT: &str = "/other";
/// Contents of the file every test starts from.
const FILE_CONTENTS: &[u8] = b"hello";
/// Symlink target, relative so that both subjects see the same length.
const LINK_TARGET: &str = "dir/file.txt";

/// Creates a symlink at the first path pointing at the second.
type SymlinkFn = Box<dyn Fn(&Path, &Path)>;

/// One implementation under test, rooted in its own scratch tree.
struct Subject {
    /// Name reported when an assertion fails.
    name: &'static str,
    /// Implementation under test.
    fs: Arc<dyn FileOps>,
    /// Directory every path in the test is relative to.
    root: PathBuf,
    /// Creates a symlink, which [`FileOps`] deliberately cannot do.
    symlink: SymlinkFn,
    /// Kept alive so the temporary directory outlives the test.
    _tempdir: Option<TempDir>,
}

impl Subject {
    /// Absolute path of `relative` inside this subject's root.
    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// Metadata of `relative`, failing the test when it cannot be read.
    fn metadata(&self, relative: &str) -> EntryMetadata {
        self.fs
            .metadata(&self.path(relative))
            .unwrap_or_else(|e| panic!("{}: metadata {relative}: {e}", self.name))
    }
}

/// The in-memory subject: two roots on different devices, plus the shared tree.
fn fake_subject() -> Subject {
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
fn std_subject() -> Subject {
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

/// Both subjects, each populated with `dir/file.txt` and a `link` pointing at it.
fn subjects() -> Vec<Subject> {
    let mut built = Vec::new();
    for subject in [fake_subject(), std_subject()] {
        let dir = subject.path("dir");
        subject.fs.create_dir_all(&dir).unwrap_or_else(|e| panic!("{}: create_dir_all: {e}", subject.name));
        subject
            .fs
            .write_atomic(&subject.path(LINK_TARGET), FILE_CONTENTS)
            .unwrap_or_else(|e| panic!("{}: write_atomic: {e}", subject.name));
        (subject.symlink)(&subject.path("link"), Path::new(LINK_TARGET));
        built.push(subject);
    }
    built
}

#[test]
fn metadata_of_a_file_reports_a_plain_regular_file() {
    for subject in subjects() {
        let meta = subject.metadata(LINK_TARGET);

        assert!(!meta.is_dir, "{}", subject.name);
        assert!(!meta.is_symlink, "{}", subject.name);
        assert_eq!(meta.size_bytes, FILE_CONTENTS.len() as u64, "{}", subject.name);
        assert_eq!(meta.link_count, 1, "{}", subject.name);
        assert!(meta.modified.is_some(), "{}", subject.name);
        assert!(meta.accessed.is_some(), "{}", subject.name);
        assert!(meta.inode > 0, "{}", subject.name);
        assert!(meta.allocated_bytes >= meta.size_bytes, "{}", subject.name);
    }
}

#[test]
fn metadata_of_a_directory_reports_a_directory() {
    for subject in subjects() {
        let meta = subject.metadata("dir");

        assert!(meta.is_dir, "{}", subject.name);
        assert!(!meta.is_symlink, "{}", subject.name);
        assert!(meta.link_count >= 1, "{}", subject.name);
    }
}

#[test]
fn metadata_of_a_symlink_does_not_follow_it() {
    for subject in subjects() {
        let meta = subject.metadata("link");

        assert!(meta.is_symlink, "{}", subject.name);
        assert!(!meta.is_dir, "{}", subject.name);
        assert_eq!(meta.size_bytes, LINK_TARGET.len() as u64, "{}", subject.name);
    }
}

#[test]
fn everything_in_one_root_shares_a_device() {
    for subject in subjects() {
        let file = subject.metadata(LINK_TARGET);
        let dir = subject.metadata("dir");

        assert_eq!(file.device, dir.device, "{}", subject.name);
    }
}

#[test]
fn metadata_of_a_missing_path_reports_the_target_as_not_found() {
    for subject in subjects() {
        let err = subject.fs.metadata(&subject.path("ghost")).err();

        assert!(matches!(err, Some(BrozaError::TargetNotFound(_))), "{}: {err:?}", subject.name);
    }
}

#[test]
fn exists_sees_a_symlink_without_following_it() {
    for subject in subjects() {
        assert!(subject.fs.exists(&subject.path("link")), "{}", subject.name);
        subject
            .fs
            .remove_tree(&subject.path(LINK_TARGET))
            .unwrap_or_else(|e| panic!("{}: remove_tree: {e}", subject.name));

        assert!(subject.fs.exists(&subject.path("link")), "{}: dangling link", subject.name);
        assert!(!subject.fs.exists(&subject.path(LINK_TARGET)), "{}", subject.name);
        assert!(!subject.fs.exists(&subject.path("ghost")), "{}", subject.name);
    }
}

#[test]
fn remove_tree_on_a_symlink_removes_the_link_and_not_its_target() {
    for subject in subjects() {
        subject
            .fs
            .remove_tree(&subject.path("link"))
            .unwrap_or_else(|e| panic!("{}: remove_tree: {e}", subject.name));

        assert!(!subject.fs.exists(&subject.path("link")), "{}", subject.name);
        assert!(subject.fs.exists(&subject.path(LINK_TARGET)), "{}: target destroyed", subject.name);
    }
}

#[test]
fn remove_tree_on_a_directory_removes_its_contents() {
    for subject in subjects() {
        subject
            .fs
            .remove_tree(&subject.path("dir"))
            .unwrap_or_else(|e| panic!("{}: remove_tree: {e}", subject.name));

        assert!(!subject.fs.exists(&subject.path("dir")), "{}", subject.name);
        assert!(!subject.fs.exists(&subject.path(LINK_TARGET)), "{}", subject.name);
    }
}

#[test]
fn remove_tree_of_a_missing_path_reports_the_target_as_not_found() {
    for subject in subjects() {
        let err = subject.fs.remove_tree(&subject.path("ghost")).err();

        assert!(matches!(err, Some(BrozaError::TargetNotFound(_))), "{}: {err:?}", subject.name);
    }
}

#[test]
fn write_atomic_replaces_the_contents_of_an_existing_file() {
    for subject in subjects() {
        let path = subject.path(LINK_TARGET);

        subject.fs.write_atomic(&path, b"replaced").unwrap_or_else(|e| panic!("{}: {e}", subject.name));

        let read = subject.fs.read(&path).unwrap_or_else(|e| panic!("{}: {e}", subject.name));
        assert_eq!(read, b"replaced", "{}", subject.name);
        assert_eq!(subject.metadata(LINK_TARGET).size_bytes, 8, "{}", subject.name);
    }
}

#[test]
fn read_of_a_missing_file_reports_the_target_as_not_found() {
    for subject in subjects() {
        let err = subject.fs.read(&subject.path("ghost")).err();

        assert!(matches!(err, Some(BrozaError::TargetNotFound(_))), "{}: {err:?}", subject.name);
    }
}

#[test]
fn read_dir_lists_the_direct_children_only() {
    for subject in subjects() {
        let mut names = subject
            .fs
            .read_dir(&subject.root)
            .unwrap_or_else(|e| panic!("{}: read_dir: {e}", subject.name))
            .iter()
            .filter_map(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
            .collect::<Vec<_>>();
        names.sort();

        assert_eq!(names, vec!["dir".to_owned(), "link".to_owned()], "{}", subject.name);
    }
}

#[test]
fn read_dir_of_a_missing_directory_reports_the_target_as_not_found() {
    for subject in subjects() {
        let err = subject.fs.read_dir(&subject.path("ghost")).err();

        assert!(matches!(err, Some(BrozaError::TargetNotFound(_))), "{}: {err:?}", subject.name);
    }
}

#[test]
fn rename_moves_an_entry_within_one_device() {
    for subject in subjects() {
        let from = subject.path(LINK_TARGET);
        let to = subject.path("dir/renamed.txt");

        subject.fs.rename(&from, &to).unwrap_or_else(|e| panic!("{}: rename: {e}", subject.name));

        assert!(!subject.fs.exists(&from), "{}", subject.name);
        let read = subject.fs.read(&to).unwrap_or_else(|e| panic!("{}: {e}", subject.name));
        assert_eq!(read, FILE_CONTENTS, "{}", subject.name);
    }
}

#[test]
fn the_fake_refuses_a_rename_across_devices_like_exdev() {
    let subject = fake_subject();
    subject.fs.create_dir_all(Path::new(FAKE_ROOT)).unwrap_or_else(|e| panic!("{e}"));
    subject.fs.create_dir_all(Path::new(FAKE_OTHER_ROOT)).unwrap_or_else(|e| panic!("{e}"));
    let from = Path::new(FAKE_ROOT).join("file.txt");
    let to = Path::new(FAKE_OTHER_ROOT).join("file.txt");
    subject.fs.write_atomic(&from, FILE_CONTENTS).unwrap_or_else(|e| panic!("{e}"));

    let err = subject.fs.rename(&from, &to).err();

    let Some(BrozaError::Io { context, source }) = err else { panic!("expected BrozaError::Io") };
    assert!(context.contains("across devices"), "{context}");
    assert_eq!(source.raw_os_error(), Some(18), "EXDEV");
    assert!(subject.fs.exists(&from), "the source must survive a failed rename");
}
