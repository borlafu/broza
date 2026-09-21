//! Contract tests for the error shapes POSIX defines.
//!
//! A cleanup tool that mistakes "this is a directory" for "this does not exist"
//! deletes the wrong thing, so the fake has to fail exactly where and how the real
//! filesystem fails. Every errno asserted here was measured on APFS.
#![cfg(feature = "test-support")]

mod fs_subjects;

use fs_subjects::{EEXIST, EISDIR, ENOTDIR, ENOTEMPTY, FILE, FILE_CONTENTS, OTHER_CONTENTS, subjects};

#[test]
fn a_directory_cannot_be_created_under_a_file() {
    for subject in subjects() {
        let result = subject.fs.create_dir_all(&subject.path("dir/file.txt/child"));

        subject.expect_errno("create_dir_all under a file", result, ENOTDIR);
        assert_eq!(subject.read(FILE), FILE_CONTENTS, "{}", subject.name);
    }
}

#[test]
fn a_directory_cannot_be_created_where_a_file_already_is() {
    for subject in subjects() {
        let result = subject.fs.create_dir_all(&subject.path(FILE));

        subject.expect_errno("create_dir_all over a file", result, EEXIST);
    }
}

#[test]
fn creating_an_existing_directory_succeeds() {
    for subject in subjects() {
        let result = subject.fs.create_dir_all(&subject.path("dir"));

        assert!(result.is_ok(), "{}: {result:?}", subject.name);
    }
}

#[test]
fn writing_over_a_directory_fails_and_leaves_it_alone() {
    for subject in subjects() {
        let result = subject.fs.write_atomic(&subject.path("dir"), b"x");

        subject.expect_errno("write_atomic over a directory", result, EISDIR);
        assert!(subject.metadata("dir").is_dir, "{}", subject.name);
        assert_eq!(subject.read(FILE), FILE_CONTENTS, "{}", subject.name);
    }
}

#[test]
fn writing_into_a_missing_directory_reports_the_directory_as_not_found() {
    for subject in subjects() {
        let result = subject.fs.write_atomic(&subject.path("missing/file"), b"x");

        subject.expect_not_found("write_atomic into a missing directory", &result);
    }
}

#[test]
fn a_file_cannot_be_renamed_onto_a_directory() {
    for subject in subjects() {
        let result = subject.fs.rename(&subject.path(FILE), &subject.path("empty"));

        subject.expect_errno("rename a file onto a directory", result, EISDIR);
        assert_eq!(subject.read(FILE), FILE_CONTENTS, "{}", subject.name);
    }
}

#[test]
fn a_directory_cannot_be_renamed_onto_a_file() {
    for subject in subjects() {
        let result = subject.fs.rename(&subject.path("empty"), &subject.path("other.txt"));

        subject.expect_errno("rename a directory onto a file", result, ENOTDIR);
        assert_eq!(subject.read("other.txt"), OTHER_CONTENTS, "{}", subject.name);
    }
}

#[test]
fn a_directory_cannot_be_renamed_onto_a_directory_that_still_has_children() {
    for subject in subjects() {
        let result = subject.fs.rename(&subject.path("empty"), &subject.path("full"));

        subject.expect_errno("rename onto a non-empty directory", result, ENOTEMPTY);
        assert_eq!(subject.child_names("full"), vec!["keep.txt".to_owned()], "{}", subject.name);
    }
}

#[test]
fn renaming_onto_an_existing_file_replaces_it() {
    for subject in subjects() {
        let result = subject.fs.rename(&subject.path(FILE), &subject.path("other.txt"));

        assert!(result.is_ok(), "{}: {result:?}", subject.name);
        assert_eq!(subject.read("other.txt"), FILE_CONTENTS, "{}", subject.name);
        assert!(!subject.fs.exists(&subject.path(FILE)), "{}", subject.name);
    }
}

#[test]
fn renaming_into_a_missing_directory_reports_the_directory_as_not_found() {
    for subject in subjects() {
        let result = subject.fs.rename(&subject.path(FILE), &subject.path("missing/file.txt"));

        subject.expect_not_found("rename into a missing directory", &result);
        assert_eq!(subject.read(FILE), FILE_CONTENTS, "{}", subject.name);
    }
}

#[test]
fn renaming_under_a_file_fails_because_a_file_is_not_a_directory() {
    for subject in subjects() {
        let result = subject.fs.rename(&subject.path(FILE), &subject.path("other.txt/child"));

        subject.expect_errno("rename under a file", result, ENOTDIR);
        assert_eq!(subject.read(FILE), FILE_CONTENTS, "{}", subject.name);
    }
}

#[test]
fn renaming_a_missing_entry_reports_the_source_as_not_found() {
    for subject in subjects() {
        let result = subject.fs.rename(&subject.path("ghost"), &subject.path("dir/ghost"));

        subject.expect_not_found("rename a missing entry", &result);
    }
}

#[test]
fn a_directory_cannot_be_read_as_a_file() {
    for subject in subjects() {
        let result = subject.fs.read(&subject.path("dir"));

        subject.expect_errno("read a directory", result, EISDIR);
    }
}

#[test]
fn a_file_cannot_be_read_as_a_directory() {
    for subject in subjects() {
        let result = subject.fs.read_dir(&subject.path(FILE));

        subject.expect_errno("read_dir a file", result, ENOTDIR);
    }
}

#[test]
fn an_exclusive_directory_creation_reports_a_taken_name_the_same_way() {
    for subject in subjects() {
        let result = subject.fs.create_dir_exclusive(&subject.path("dir"));

        subject.expect_errno("create_dir_exclusive over a directory", result, EEXIST);
    }
}

#[test]
fn an_exclusive_rename_reports_a_taken_destination_the_same_way() {
    for subject in subjects() {
        let result = subject.fs.rename_exclusive(&subject.path(FILE), &subject.path("other.txt"));

        subject.expect_errno("rename_exclusive over a file", result, EEXIST);
    }
}

#[test]
fn an_exclusive_rename_of_a_missing_entry_reports_the_source_as_not_found() {
    for subject in subjects() {
        let result = subject.fs.rename_exclusive(&subject.path("ghost"), &subject.path("dir/ghost"));

        subject.expect_not_found("rename_exclusive a missing entry", &result);
    }
}

#[test]
fn renaming_a_path_onto_itself_changes_nothing_and_succeeds() {
    for subject in subjects() {
        let result = subject.fs.rename_exclusive(&subject.path(FILE), &subject.path(FILE));

        assert!(result.is_ok(), "{}: rename_exclusive onto itself: {result:?}", subject.name);
        assert_eq!(
            subject.fs.read(&subject.path(FILE)).ok().as_deref(),
            Some(FILE_CONTENTS),
            "{}: the file is untouched",
            subject.name
        );
    }
}

#[test]
fn a_lock_is_exclusive_until_it_is_dropped() {
    for subject in subjects() {
        let path = subject.path("session.lock");

        let held = subject.fs.lock_exclusive(&path);
        let second = subject.fs.lock_exclusive(&path);

        assert!(held.is_ok(), "{}: the first lock has to be granted", subject.name);
        assert!(
            second.as_ref().err().is_some_and(broza::ports::is_busy),
            "{}: the second must be told it is busy, not granted",
            subject.name
        );
        drop(second);
        drop(held);
        let again = subject.fs.lock_exclusive(&path);
        assert!(again.is_ok(), "{}: after release: {:?}", subject.name, again.err());
    }
}

#[test]
fn locking_creates_the_lock_file_when_it_is_missing() {
    for subject in subjects() {
        let path = subject.path("fresh.lock");

        let held = subject.fs.lock_exclusive(&path);

        assert!(held.is_ok(), "{}: locking a missing path creates it", subject.name);
        assert!(subject.fs.exists(&path), "{}: the lock file is there", subject.name);
    }
}
