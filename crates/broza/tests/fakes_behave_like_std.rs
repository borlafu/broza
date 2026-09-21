//! Contract tests: `FakeFileOps` must be indistinguishable from `StdFileOps`.
//!
//! Every test runs against both implementations over the same synthetic tree. A fake
//! that drifts from the real filesystem turns unit tests into wishful thinking, so the
//! two are pinned together here (`docs/implementation-plan.md` §3.6). The error
//! shapes POSIX defines have their own file, `fakes_behave_like_std_posix.rs`.
//!
//! Only the `test-support` feature exposes `broza::testing`; without it this file
//! compiles to nothing.
#![cfg(feature = "test-support")]

mod fs_subjects;

use std::path::Path;

use broza::BrozaError;
use fs_subjects::{
    DIR_LINK, EXDEV, FAKE_OTHER_ROOT, FAKE_ROOT, FIFO, FILE, FILE_CONTENTS, FILE_LINK, FORKED_CONTENTS,
    FORKED_FILE, SPECIAL_DIR, fake_subject, std_subject, subjects,
};

#[test]
fn metadata_of_a_file_reports_a_plain_regular_file() {
    for subject in subjects() {
        let meta = subject.metadata(FILE);

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
fn a_hard_link_shares_the_inode_and_raises_the_link_count() {
    for subject in subjects() {
        subject.hard_link(FILE, "second-name");

        let original = subject.metadata(FILE);
        let link = subject.metadata("second-name");

        assert_eq!(original.inode, link.inode, "{}", subject.name);
        assert_eq!(original.link_count, 2, "{}", subject.name);
        assert_eq!(link.link_count, 2, "{}", subject.name);
        assert_eq!(link.size_bytes, original.size_bytes, "{}", subject.name);
    }
}

#[test]
fn a_listing_with_metadata_says_the_same_as_stating_every_child() {
    for subject in subjects() {
        let listing = subject
            .fs
            .read_dir_with_metadata(&subject.path("dir"))
            .unwrap_or_else(|e| panic!("{}: {e}", subject.name));

        assert!(!listing.is_empty(), "{}", subject.name);
        for (path, meta) in listing {
            let bulk = meta.unwrap_or_else(|e| panic!("{}: {} {e}", subject.name, path.display()));
            let stated = subject
                .fs
                .metadata(&path)
                .unwrap_or_else(|e| panic!("{}: {} {e}", subject.name, path.display()));
            assert_eq!(bulk, stated, "{}: {}", subject.name, path.display());
        }
    }
}

#[test]
fn a_listing_holds_exactly_the_children_read_dir_reports() {
    for subject in subjects() {
        let entries = subject
            .fs
            .read_dir_with_metadata(&subject.path("."))
            .unwrap_or_else(|e| panic!("{}: {e}", subject.name));
        let mut listed: Vec<_> = entries.into_iter().map(|(path, _)| path).collect();
        let mut expected = subject.children(".");
        listed.sort();
        expected.sort();

        assert_eq!(listed, expected, "{}", subject.name);
    }
}

#[test]
fn both_readers_of_the_real_filesystem_answer_the_same_thing() {
    // The fast reader is a different code path, not a different answer: reset
    // what it has learned, make it read the tree, and compare it against the
    // plain `lstat` of every entry — resource fork, FIFO and all.
    broza::adapters::reset_bulk_state_for_tests();
    let subject = std_subject();
    let subject = fs_subjects::populate_for_test(subject);

    for directory in ["dir", SPECIAL_DIR, "empty", ""] {
        let listing = subject
            .fs
            .read_dir_with_metadata(&subject.path(directory))
            .unwrap_or_else(|e| panic!("{directory}: {e}"));
        for (path, meta) in listing {
            let fast = meta.unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let stated = subject.fs.metadata(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_eq!(fast, stated, "{}", path.display());
        }
    }
}

#[test]
fn a_file_with_a_resource_fork_is_measured_the_way_lstat_measures_it() {
    broza::adapters::reset_bulk_state_for_tests();
    let subject = fs_subjects::populate_for_test(std_subject());
    let forked = subject.path(FORKED_FILE);

    let listing =
        subject.fs.read_dir_with_metadata(&subject.path(SPECIAL_DIR)).unwrap_or_else(|e| panic!("{e}"));
    let entry = listing
        .into_iter()
        .find(|(path, _)| path == &forked)
        .unwrap_or_else(|| panic!("no forked file in the listing"));
    let fast = entry.1.unwrap_or_else(|e| panic!("{e}"));
    let stated = subject.fs.metadata(&forked).unwrap_or_else(|e| panic!("{e}"));

    // `st_size` is the data fork; the resource fork shows up in the blocks.
    assert_eq!(fast.size_bytes, FORKED_CONTENTS.len() as u64);
    assert_eq!(fast, stated);
    assert!(stated.allocated_bytes > stated.size_bytes, "the fork takes room: {stated:?}");
}

#[test]
fn a_fifo_in_the_tree_is_listed_and_not_opened() {
    broza::adapters::reset_bulk_state_for_tests();
    let subject = fs_subjects::populate_for_test(std_subject());
    let fifo = subject.path(FIFO);

    let listing =
        subject.fs.read_dir_with_metadata(&subject.path(SPECIAL_DIR)).unwrap_or_else(|e| panic!("{e}"));

    let entry = listing
        .into_iter()
        .find(|(path, _)| path == &fifo)
        .unwrap_or_else(|| panic!("no fifo in the listing"));
    let meta = entry.1.unwrap_or_else(|e| panic!("{e}"));
    assert!(!meta.is_dir);
    assert_eq!(meta, subject.fs.metadata(&fifo).unwrap_or_else(|e| panic!("{e}")));
}

#[test]
fn nothing_in_a_scratch_tree_is_a_cloud_placeholder() {
    for subject in subjects() {
        assert!(!subject.metadata(FILE).is_dataless, "{}", subject.name);
        assert!(!subject.metadata("dir").is_dataless, "{}", subject.name);
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
        let meta = subject.metadata(FILE_LINK);

        assert!(meta.is_symlink, "{}", subject.name);
        assert!(!meta.is_dir, "{}", subject.name);
        assert_eq!(meta.size_bytes, FILE.len() as u64, "{}", subject.name);
    }
}

#[test]
fn metadata_of_a_symlink_to_a_directory_is_still_a_symlink() {
    for subject in subjects() {
        let meta = subject.metadata(DIR_LINK);

        assert!(meta.is_symlink, "{}", subject.name);
        assert!(!meta.is_dir, "{}", subject.name);
    }
}

#[test]
fn everything_in_one_root_shares_a_device() {
    for subject in subjects() {
        let file = subject.metadata(FILE);
        let dir = subject.metadata("dir");

        assert_eq!(file.device, dir.device, "{}", subject.name);
    }
}

#[test]
fn metadata_of_a_missing_path_reports_the_target_as_not_found() {
    for subject in subjects() {
        let result = subject.fs.metadata(&subject.path("ghost"));

        subject.expect_not_found("metadata", &result);
    }
}

#[test]
fn exists_sees_a_symlink_without_following_it() {
    for subject in subjects() {
        assert!(subject.fs.exists(&subject.path(FILE_LINK)), "{}", subject.name);
        subject
            .fs
            .remove_tree(&subject.path(FILE))
            .unwrap_or_else(|e| panic!("{}: remove_tree: {e}", subject.name));

        assert!(subject.fs.exists(&subject.path(FILE_LINK)), "{}: dangling link", subject.name);
        assert!(!subject.fs.exists(&subject.path(FILE)), "{}", subject.name);
        assert!(!subject.fs.exists(&subject.path("ghost")), "{}", subject.name);
    }
}

#[test]
fn remove_tree_on_a_symlink_removes_the_link_and_not_its_target() {
    for subject in subjects() {
        subject
            .fs
            .remove_tree(&subject.path(FILE_LINK))
            .unwrap_or_else(|e| panic!("{}: remove_tree: {e}", subject.name));

        assert!(!subject.fs.exists(&subject.path(FILE_LINK)), "{}", subject.name);
        assert!(subject.fs.exists(&subject.path(FILE)), "{}: target destroyed", subject.name);
    }
}

#[test]
fn remove_tree_on_a_symlink_to_a_directory_keeps_the_directory() {
    for subject in subjects() {
        subject
            .fs
            .remove_tree(&subject.path(DIR_LINK))
            .unwrap_or_else(|e| panic!("{}: remove_tree: {e}", subject.name));

        assert!(!subject.fs.exists(&subject.path(DIR_LINK)), "{}", subject.name);
        assert!(subject.fs.exists(&subject.path(FILE)), "{}: target destroyed", subject.name);
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
        assert!(!subject.fs.exists(&subject.path(FILE)), "{}", subject.name);
    }
}

#[test]
fn remove_tree_of_a_missing_path_reports_the_target_as_not_found() {
    for subject in subjects() {
        let result = subject.fs.remove_tree(&subject.path("ghost"));

        subject.expect_not_found("remove_tree", &result);
    }
}

#[test]
fn write_atomic_replaces_the_contents_of_an_existing_file() {
    for subject in subjects() {
        let path = subject.path(FILE);

        subject.fs.write_atomic(&path, b"replaced").unwrap_or_else(|e| panic!("{}: {e}", subject.name));

        assert_eq!(subject.read(FILE), b"replaced", "{}", subject.name);
        assert_eq!(subject.metadata(FILE).size_bytes, 8, "{}", subject.name);
    }
}

#[test]
fn read_of_a_missing_file_reports_the_target_as_not_found() {
    for subject in subjects() {
        let result = subject.fs.read(&subject.path("ghost"));

        subject.expect_not_found("read", &result);
    }
}

#[test]
fn read_follows_a_symlink_to_its_target() {
    for subject in subjects() {
        assert_eq!(subject.read(FILE_LINK), FILE_CONTENTS, "{}", subject.name);
    }
}

#[test]
fn read_follows_a_symlink_in_the_middle_of_a_path() {
    for subject in subjects() {
        assert_eq!(subject.read("dirlink/file.txt"), FILE_CONTENTS, "{}", subject.name);
    }
}

#[test]
fn read_dir_lists_the_direct_children_only() {
    for subject in subjects() {
        let names = subject.child_names("");

        assert_eq!(
            names,
            ["dir", "dirlink", "empty", "full", "link", "other.txt", "special"],
            "{}",
            subject.name
        );
    }
}

#[test]
fn read_dir_through_a_symlink_reports_the_children_under_the_path_asked_for() {
    for subject in subjects() {
        let children = subject.children(DIR_LINK);

        assert_eq!(children, vec![subject.path("dirlink/file.txt")], "{}", subject.name);
    }
}

#[test]
fn read_dir_of_a_missing_directory_reports_the_target_as_not_found() {
    for subject in subjects() {
        let result = subject.fs.read_dir(&subject.path("ghost"));

        subject.expect_not_found("read_dir", &result);
    }
}

#[test]
fn rename_moves_an_entry_within_one_device() {
    for subject in subjects() {
        let from = subject.path(FILE);
        let to = subject.path("dir/renamed.txt");

        subject.fs.rename(&from, &to).unwrap_or_else(|e| panic!("{}: rename: {e}", subject.name));

        assert!(!subject.fs.exists(&from), "{}", subject.name);
        assert_eq!(subject.read("dir/renamed.txt"), FILE_CONTENTS, "{}", subject.name);
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
    assert_eq!(source.raw_os_error(), Some(EXDEV));
    assert!(subject.fs.exists(&from), "the source must survive a failed rename");
}
