//! From one walk to the records that can stand in for it next time.
//!
//! A record on its own is an aggregate; what makes a subtree servable without
//! a walk is that every record also names the directories and the big files
//! directly inside it, so the store can rebuild the subtree node by node
//! (`docs/adr/0008-cache-records-carry-their-children.md`).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::scan::cache::key::{CACHE_FILE_FLOOR_BYTES, CacheKey, ChildDir, DirRecord, FileRecord};
use crate::scan::walker::{DirNode, FileEntry, WalkResult};

/// The records of every directory the walk measured itself.
///
/// A directory that came from the cache is not recorded again (its record is
/// the one it came from), and a truncated one is not recorded at all, as
/// [`DirRecord::of`] decides. The child lists and file lists are sorted by
/// name so two walks of one tree write byte-identical stores.
pub fn records_of(walk: &WalkResult, recorded_at: Timestamp) -> Vec<DirRecord> {
    let mut children: HashMap<&Path, Vec<ChildDir>> = HashMap::new();
    for node in &walk.nodes {
        if let (Some(parent), Some(name)) = (node.path.parent(), node.path.file_name()) {
            children.entry(parent).or_default().push(ChildDir {
                name: name.as_bytes().to_vec(),
                device: node.device,
                inode: node.inode,
                mtime_ns: node.mtime.map(Timestamp::as_nanosecond),
            });
        }
    }
    let mut files: HashMap<&Path, Vec<FileRecord>> = HashMap::new();
    for file in walk.cache_files.iter().filter(|file| is_kept(file)) {
        if let (Some(parent), Some(name)) = (file.path.parent(), file.path.file_name()) {
            files.entry(parent).or_default().push(FileRecord {
                name: name.as_bytes().to_vec(),
                size_bytes: file.size_bytes,
                allocated_bytes: file.allocated_bytes,
                inode: file.inode,
                link_count: file.link_count,
                modified_ns: file.modified.map(Timestamp::as_nanosecond),
                accessed_ns: file.accessed.map(Timestamp::as_nanosecond),
            });
        }
    }
    walk.nodes
        .iter()
        .filter_map(|node| {
            let record = DirRecord::of(node, recorded_at)?;
            let mut child_dirs = children.remove(node.path.as_path()).unwrap_or_default();
            child_dirs.sort_by(|a, b| a.name.cmp(&b.name));
            let mut own_files = files.remove(node.path.as_path()).unwrap_or_default();
            own_files.sort_by(|a, b| a.name.cmp(&b.name));
            Some(record.with_child_dirs(child_dirs).with_files(own_files))
        })
        .collect()
}

/// Either size at or above the floor, like the walk's own report.
fn is_kept(file: &FileEntry) -> bool {
    file.size_bytes.max(file.allocated_bytes) >= CACHE_FILE_FLOOR_BYTES
}

/// The node a record stands for, at `path`, marked as served from the cache.
pub(super) fn node_of(path: &Path, record: &DirRecord) -> DirNode {
    DirNode {
        path: path.to_path_buf(),
        size_bytes: record.size_bytes,
        allocated_bytes: record.allocated_bytes,
        file_count: record.file_count,
        dir_count: record.dir_count,
        dataless_count: record.dataless_count,
        largest_item_bytes: record.largest_item_bytes,
        has_hard_links: record.has_hard_links,
        has_truncation: record.has_truncation,
        device: record.key.device,
        inode: record.key.inode,
        mtime: Timestamp::from_nanosecond(record.key.mtime_ns).ok(),
        children_truncated: false,
        from_cache: true,
    }
}

/// The file entry a record stands for, directly inside `dir`.
pub(super) fn file_of(dir: &Path, device: u64, record: &FileRecord) -> FileEntry {
    FileEntry {
        path: dir.join(OsStr::from_bytes(&record.name)),
        size_bytes: record.size_bytes,
        allocated_bytes: record.allocated_bytes,
        device,
        inode: record.inode,
        link_count: record.link_count,
        modified: record.modified_ns.and_then(|ns| Timestamp::from_nanosecond(ns).ok()),
        accessed: record.accessed_ns.and_then(|ns| Timestamp::from_nanosecond(ns).ok()),
    }
}

/// The key a child entry's own record has.
pub(super) fn child_key(child: &ChildDir) -> Option<CacheKey> {
    Some(CacheKey { device: child.device, inode: child.inode, mtime_ns: child.mtime_ns? })
}

/// `true` for a name that is one plain path component: what `read_dir` gives
/// and what `records_of` writes. A store is a plain file, and a record that
/// names `..`, an empty string or a slash would place a node outside the
/// subtree it came from, so it is not served.
pub(super) fn is_plain_name(name: &[u8]) -> bool {
    !name.is_empty() && name != b"." && name != b".." && !name.contains(&b'/')
}

/// The path a child entry lives at.
pub(super) fn child_path(parent: &Path, child: &ChildDir) -> PathBuf {
    parent.join(OsStr::from_bytes(&child.name))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use jiff::Timestamp;

    use super::records_of;
    use crate::scan::walker::{WalkOptions, walk};
    use crate::testing::FakeFileOps;

    #[test]
    fn a_record_names_its_child_directories_and_its_big_files_in_name_order() {
        let fs = FakeFileOps::new()
            .with_root("/vol", 1)
            .with_sized_file("/vol/a/zeta.bin", 3_000_000)
            .with_sized_file("/vol/a/alpha.bin", 2_000_000)
            .with_sized_file("/vol/a/small.txt", 10)
            .with_sized_file("/vol/a/sub/x", 1)
            .with_sized_file("/vol/a/dir2/y", 1);
        let options = WalkOptions { cache_files_min_size: Some(1_000_000), ..WalkOptions::default() };
        let walked = walk(Path::new("/vol"), &options, &fs);

        let records = records_of(&walked, Timestamp::UNIX_EPOCH);

        let a_node =
            walked.nodes.iter().find(|node| node.path == Path::new("/vol/a")).unwrap_or_else(|| panic!("a"));
        let a = records
            .iter()
            .find(|record| record.key.inode == a_node.inode)
            .unwrap_or_else(|| panic!("record"));
        let children: Vec<&[u8]> = a.child_dirs.iter().map(|child| child.name.as_slice()).collect();
        assert_eq!(children, vec![b"dir2".as_slice(), b"sub".as_slice()]);
        let files: Vec<&[u8]> = a.files.iter().map(|file| file.name.as_slice()).collect();
        assert_eq!(
            files,
            vec![b"alpha.bin".as_slice(), b"zeta.bin".as_slice()],
            "small.txt is under the floor"
        );
        assert_eq!(a.files[0].size_bytes, 2_000_000);
    }
}
