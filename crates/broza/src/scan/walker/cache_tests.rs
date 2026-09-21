//! What the cache may and may not answer for, from the walker's side.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::tests::{node, record, sample, walk_sample};
use crate::scan::walker::{DirIdentity, WalkOptions};

#[test]
fn a_cache_hit_reuses_the_whole_aggregate_and_does_not_descend() {
    let seen: Mutex<Vec<DirIdentity>> = Mutex::new(Vec::new());
    let hook = |identity: &DirIdentity| {
        seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(identity.clone());
        (identity.path == Path::new("/vol/a")).then(|| record(identity, 777))
    };
    let options = WalkOptions { skip_hook: Some(&hook), ..WalkOptions::default() };

    let result = walk_sample(&sample(), &options);

    assert_eq!(result.paths(), vec![PathBuf::from("/vol"), PathBuf::from("/vol/a"), PathBuf::from("/vol/b")]);
    let cached = node(&result, "/vol/a");
    assert_eq!(cached.size_bytes, 777);
    assert_eq!(cached.allocated_bytes, 1554);
    assert_eq!(cached.file_count, 3);
    assert_eq!(cached.dir_count, 1);
    assert!(!cached.children_truncated, "a cached subtree was measured, not truncated");
    let root = node(&result, "/vol");
    assert_eq!(root.size_bytes, 5777);
    assert_eq!(root.file_count, 4, "three cached files plus the one under /vol/b");
    assert_eq!(root.dir_count, 3, "a and b, plus the one inside a");
    let asked = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(asked.iter().all(|identity| identity.device == 1));
    assert!(asked.iter().any(|identity| identity.path == Path::new("/vol/a")));
}

#[test]
fn the_cache_is_never_asked_about_the_root_itself() {
    let hook = |identity: &DirIdentity| Some(record(identity, 1));
    let options = WalkOptions { skip_hook: Some(&hook), ..WalkOptions::default() };

    let result = walk_sample(&sample(), &options);

    // Answering at the root would report a volume with no contents at all.
    assert_eq!(node(&result, "/vol").size_bytes, 2, "both children came from the cache");
    assert_eq!(result.paths().len(), 3);
}

#[test]
fn the_cache_can_be_held_off_until_a_deeper_level() {
    let hook = |identity: &DirIdentity| Some(record(identity, 1));
    let options = WalkOptions { skip_hook: Some(&hook), cache_from_depth: 2, ..WalkOptions::default() };

    let result = walk_sample(&sample(), &options);

    assert_eq!(
        result.paths(),
        vec![
            PathBuf::from("/vol"),
            PathBuf::from("/vol/a"),
            PathBuf::from("/vol/a/sub"),
            PathBuf::from("/vol/b"),
        ],
        "the first two levels are walked whatever the cache says"
    );
    assert_eq!(node(&result, "/vol/a/sub").size_bytes, 1, "the third level is cached");
    assert_eq!(node(&result, "/vol/a").size_bytes, 3001, "its own files, plus the cached subtree");
}
