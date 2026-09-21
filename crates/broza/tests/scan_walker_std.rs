//! The walker over the real filesystem.
//!
//! Unit tests run against `FakeFileOps`; this file is the other half of that
//! bargain (`docs/implementation-plan.md` §3.6): the same walker, the same
//! assertions, over a temporary directory built with `std::fs`, including the two
//! things only a real filesystem provides — a hard link and a directory the
//! process may not read.
#![cfg(feature = "test-support")]

use std::fs;
use std::path::{Path, PathBuf};

use broza::adapters::StdFileOps;
use broza::scan::walker::{PERMISSION_DENIED_CODE, WalkOptions, WalkResult, walk};

/// Sizes of the files the sample tree is built from.
const FILES: [(&str, usize); 4] = [("a/f1", 1000), ("a/f2", 2000), ("a/sub/f3", 3000), ("b/big", 5000)];
/// Size of the file hidden inside the unreadable directory.
const LOCKED_FILE_BYTES: usize = 4000;
/// Target of the symlink, and therefore its apparent size in bytes.
const SYMLINK_TARGET: &str = "b";
/// Apparent bytes the walker must report for the whole tree.
const EXPECTED_BYTES: u64 = 11_001;

/// Build the sample tree and return its root.
fn sample_tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    for (relative, size) in FILES {
        let path = dir.path().join(relative);
        let parent = path.parent().unwrap_or_else(|| panic!("no parent for {relative}"));
        fs::create_dir_all(parent).unwrap_or_else(|e| panic!("mkdir {}: {e}", parent.display()));
        fs::write(&path, vec![0_u8; size]).unwrap_or_else(|e| panic!("write {relative}: {e}"));
    }
    fs::hard_link(dir.path().join("a/f1"), dir.path().join("b/f1-link"))
        .unwrap_or_else(|e| panic!("hard link: {e}"));
    std::os::unix::fs::symlink(SYMLINK_TARGET, dir.path().join("a/link"))
        .unwrap_or_else(|e| panic!("symlink: {e}"));
    dir
}

/// Add a directory the process may not read; `false` when it stays readable (root).
fn add_locked_dir(root: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let locked = root.join("locked");
    fs::create_dir_all(&locked).unwrap_or_else(|e| panic!("mkdir locked: {e}"));
    fs::write(locked.join("hidden"), vec![0_u8; LOCKED_FILE_BYTES])
        .unwrap_or_else(|e| panic!("write hidden: {e}"));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap_or_else(|e| panic!("chmod: {e}"));
    // Running as root defeats the permission bits; the test then skips that half.
    fs::read_dir(&locked).is_err()
}

/// Restore the permissions so the temporary directory can be removed.
fn unlock(root: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let _ = fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o700));
}

fn node<'a>(result: &'a WalkResult, path: &Path) -> &'a broza::scan::walker::DirNode {
    result
        .nodes
        .iter()
        .find(|node| node.path == path)
        .unwrap_or_else(|| panic!("no node for {}", path.display()))
}

#[test]
fn a_real_tree_is_aggregated_with_hard_links_counted_once() {
    let dir = sample_tree();
    let root = dir.path();

    let result = walk(root, &WalkOptions::default(), &StdFileOps);

    let total = node(&result, root);
    assert_eq!(total.size_bytes, EXPECTED_BYTES, "{:?}", result.paths());
    assert_eq!(total.file_count, 5, "the second name of f1 must not be counted");
    assert_eq!(total.dir_count, 3);
    assert!(total.allocated_bytes >= total.size_bytes);
    assert!(total.inode > 0);
    assert!(total.mtime.is_some());
    assert_eq!(node(&result, &root.join("a/sub")).size_bytes, 3000);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

#[test]
fn a_symlink_is_counted_by_its_own_size_and_not_followed() {
    let dir = sample_tree();
    let root = dir.path();

    let result = walk(root, &WalkOptions::default(), &StdFileOps);

    // `a` holds f1, f2, the link, and the subdirectory: following the link would
    // add the 5000 bytes of `b` to it.
    assert_eq!(node(&result, &root.join("a")).size_bytes, 6000 + SYMLINK_TARGET.len() as u64);
}

#[test]
fn an_unreadable_directory_is_a_warning_and_the_rest_is_still_reported() {
    let dir = sample_tree();
    let root = dir.path();
    let locked = add_locked_dir(root);

    let result = walk(root, &WalkOptions::default(), &StdFileOps);
    unlock(root);

    if !locked {
        eprintln!("skipped: running as root, chmod 000 does not deny anything");
        return;
    }
    let denied: Vec<_> = result.errors.iter().filter(|entry| entry.code == PERMISSION_DENIED_CODE).collect();
    assert_eq!(denied.len(), 1, "{:?}", result.errors);
    assert_eq!(denied[0].path.as_deref(), Some(root.join("locked").as_path()));
    assert_eq!(node(&result, root).size_bytes, EXPECTED_BYTES, "the hidden file is not counted");
    // The directory itself can still be stated, so it is reported; only what is
    // inside it is unknown.
    let blocked = node(&result, &root.join("locked"));
    assert!(blocked.children_truncated);
    assert_eq!(blocked.size_bytes, 0);
    assert!(!node(&result, root).children_truncated, "the root's own children were all listed");
}

#[test]
fn files_above_the_threshold_come_back_with_their_real_sizes() {
    let dir = sample_tree();
    // The threshold is allocated bytes; APFS allocates 4 KiB blocks, so a
    // 3000-byte file occupies 4096 and a 5000-byte one 8192.
    let options =
        WalkOptions { report_files_min_size: Some(5000), report_files_top: 10, ..WalkOptions::default() };

    let result = walk(dir.path(), &options, &StdFileOps);

    let collected: Vec<(PathBuf, u64)> =
        result.files.iter().map(|file| (file.path.clone(), file.size_bytes)).collect();
    assert_eq!(collected, vec![(dir.path().join("b/big"), 5000)]);
}

#[test]
fn a_depth_limit_keeps_the_totals_and_drops_the_deep_nodes() {
    let dir = sample_tree();
    let options = WalkOptions { max_depth: Some(1), ..WalkOptions::default() };

    let result = walk(dir.path(), &options, &StdFileOps);

    assert_eq!(result.paths(), vec![dir.path().to_path_buf(), dir.path().join("a"), dir.path().join("b")]);
    assert_eq!(node(&result, dir.path()).size_bytes, EXPECTED_BYTES);
}
