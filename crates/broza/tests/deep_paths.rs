//! A tree deeper than a path can name is walked to the bottom.
//!
//! The kernel refuses paths of `PATH_MAX` (1024) bytes or more, yet a shell
//! loop of `mkdir d && cd d` builds a tree that deep with no trouble. The walk
//! descends by directory descriptor (ADR 0010), so it reads such a tree like
//! any other; a path-based `lstat` on the same leaf fails with
//! `ENAMETOOLONG`, which is what this test pins on both sides.

use broza::adapters::StdFileOps;
use broza::ports::FileOps;
use broza::scan::{WalkOptions, walk};

/// Levels below the temporary root: two bytes each, past 1024 in all even
/// under a short temporary directory.
const DEPTH: usize = 520;
/// `ENAMETOOLONG`.
const ENAMETOOLONG: i32 = 63;

#[test]
fn a_tree_deeper_than_path_max_is_walked_by_descriptor() {
    let root = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    // `create_dir` takes a full path and would stop at PATH_MAX itself; the
    // shell descends one short relative name at a time, like the walk.
    let script = format!(
        "cd \"$1\" && i=0; while [ $i -lt {DEPTH} ]; do mkdir d && cd d; i=$((i+1)); done && printf hello > leaf"
    );
    let built = std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .arg("sh")
        .arg(root.path())
        .status()
        .unwrap_or_else(|e| panic!("sh: {e}"));
    assert!(built.success(), "building the deep tree failed: {built}");
    let leaf = root.path().join("d/".repeat(DEPTH)).join("leaf");
    match StdFileOps.metadata(&leaf) {
        Err(broza::BrozaError::Io { source, .. }) => {
            assert_eq!(source.raw_os_error(), Some(ENAMETOOLONG), "the leaf is out of reach by path");
        }
        other => panic!("a path this long must not be statable: {other:?}"),
    }

    let result = walk(root.path(), &WalkOptions::default(), &StdFileOps);

    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.nodes.len(), DEPTH + 1, "one node per level and the root");
    let top =
        result.nodes.iter().find(|node| node.path == root.path()).unwrap_or_else(|| panic!("root node"));
    assert_eq!(top.file_count, 1);
    assert_eq!(top.size_bytes, "hello".len() as u64);
    assert_eq!(top.dir_count, DEPTH as u64);
}
