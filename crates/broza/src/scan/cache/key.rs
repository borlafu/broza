//! What the cache is keyed by, and what it stores.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::scan::walker::{DirIdentity, DirNode};

/// Smallest file a record keeps by name: the detectors' floor
/// ([`crate::scan::DETECTOR_FILES_MIN_BYTES`]), so a subtree served from the
/// cache hides no candidate from them.
pub const CACHE_FILE_FLOOR_BYTES: u64 = 1_000_000;
const _: () = assert!(CACHE_FILE_FLOOR_BYTES == crate::scan::request::DETECTOR_FILES_MIN_BYTES);

/// A directory directly inside a recorded one, by the key its own record has.
///
/// The name is the raw bytes of the entry: a path Broza did not choose need
/// not be UTF-8, and the cache must give it back exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildDir {
    /// The entry name, as bytes.
    pub name: Vec<u8>,
    /// Device id (`st_dev`) of the child; a walk never crosses devices, but
    /// the key is the child's own, not assumed from the parent.
    pub device: u64,
    /// Inode number (`st_ino`) of the child.
    pub inode: u64,
    /// Modification time of the child in nanoseconds since the unix epoch;
    /// `None` when the filesystem gave none, in which case the child has no
    /// record and the parent can never be served.
    pub mtime_ns: Option<i128>,
}

/// A file directly inside a recorded directory, at or above
/// [`CACHE_FILE_FLOOR_BYTES`]: what the walk would have reported about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    /// The entry name, as bytes.
    pub name: Vec<u8>,
    /// Apparent size in bytes.
    pub size_bytes: u64,
    /// Allocated size in bytes.
    pub allocated_bytes: u64,
    /// Inode number (`st_ino`).
    pub inode: u64,
    /// Number of hard links.
    pub link_count: u64,
    /// Last modification time in nanoseconds since the unix epoch.
    pub modified_ns: Option<i128>,
    /// Last access time in nanoseconds since the unix epoch.
    pub accessed_ns: Option<i128>,
    /// APFS clone id, when the filesystem reported one.
    pub clone_id: Option<u64>,
}

/// Identity of a directory as the cache sees it.
///
/// `mtime` is part of the key rather than a field to compare: a directory whose
/// contents changed simply has another key, and the stale record is ignored
/// instead of being trusted and then corrected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CacheKey {
    /// Device id (`st_dev`).
    pub device: u64,
    /// Inode number (`st_ino`).
    pub inode: u64,
    /// Modification time in nanoseconds since the unix epoch.
    pub mtime_ns: i128,
}

impl CacheKey {
    /// Key of `identity`; `None` when the filesystem gave no usable time.
    pub fn of(identity: &DirIdentity) -> Option<Self> {
        Some(Self {
            device: identity.device,
            inode: identity.inode,
            mtime_ns: identity.mtime?.as_nanosecond(),
        })
    }
}

/// One cached directory aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirRecord {
    /// What this record describes.
    pub key: CacheKey,
    /// Apparent bytes of the subtree.
    pub size_bytes: u64,
    /// Allocated bytes of the subtree.
    pub allocated_bytes: u64,
    /// Non-directory entries in the subtree.
    pub file_count: u64,
    /// Directories in the subtree, excluding the directory itself.
    pub dir_count: u64,
    /// Cloud placeholders in the subtree.
    pub dataless_count: u64,
    /// Biggest single reportable thing inside: the largest file, or the largest
    /// descendant directory's subtree, in allocated bytes.
    ///
    /// Kept for the node the record turns back into. Since layout 3 it is not
    /// what decides whether a subtree may be served: the request's floor
    /// against [`CACHE_FILE_FLOOR_BYTES`] is.
    pub largest_item_bytes: u64,
    /// `true` when a file with more than one name lives inside the subtree.
    /// Such a record is written but never served: see [`DirRecord::is_usable`].
    pub has_hard_links: bool,
    /// `true` when something inside the subtree was not walked. Such a record
    /// is written but never served either.
    pub has_truncation: bool,
    /// When the aggregate was measured; the TTL is counted from here.
    pub recorded_at: Timestamp,
    /// The directories directly inside, so the whole subtree can be rebuilt
    /// from the store without walking it.
    pub child_dirs: Vec<ChildDir>,
    /// The files directly inside at or above [`CACHE_FILE_FLOOR_BYTES`].
    pub files: Vec<FileRecord>,
    /// The clone families whose credited clone is directly inside, as
    /// `(device, original inode)`: a warm walk that meets another clone of one
    /// of these discounts it, as the cold walk that wrote the record did.
    pub kept_clones: Vec<(u64, u64)>,
}

impl DirRecord {
    /// `true` when this record may stand in for walking the subtree again.
    ///
    /// Two things make a subtree unfit to be served, however fresh the record
    /// is. A hard link is settled by looking at every name in one walk, so a
    /// subtree that was not walked hides names the rest of the scan would then
    /// count twice. And a subtree with a hole in it — an unreadable directory,
    /// a cloud placeholder — is both incomplete and the source of the warnings
    /// that say so, which a warm scan would otherwise lose.
    pub fn is_usable(&self) -> bool {
        !self.has_hard_links && !self.has_truncation
    }

    /// `true` when both records say the same thing about the same directory.
    ///
    /// Everything but *when* it was measured: two walks of an unchanged
    /// directory agree on every number, and a store whose records only differ
    /// in their timestamp is not worth rewriting.
    pub fn measures_the_same_as(&self, other: &Self) -> bool {
        self.key == other.key
            && self.size_bytes == other.size_bytes
            && self.allocated_bytes == other.allocated_bytes
            && self.file_count == other.file_count
            && self.dir_count == other.dir_count
            && self.dataless_count == other.dataless_count
            && self.largest_item_bytes == other.largest_item_bytes
            && self.has_hard_links == other.has_hard_links
            && self.has_truncation == other.has_truncation
            && self.child_dirs == other.child_dirs
            && self.files == other.files
    }

    /// The same record, knowing which directories are directly inside.
    #[must_use]
    pub fn with_child_dirs(self, child_dirs: Vec<ChildDir>) -> Self {
        Self { child_dirs, ..self }
    }

    /// The same record, knowing which big files are directly inside.
    #[must_use]
    pub fn with_files(self, files: Vec<FileRecord>) -> Self {
        Self { files, ..self }
    }

    /// The same record, knowing which clone families are kept directly inside.
    #[must_use]
    pub fn with_kept_clones(self, kept_clones: Vec<(u64, u64)>) -> Self {
        Self { kept_clones, ..self }
    }

    /// Record of a freshly walked directory.
    ///
    /// `None` for a node that must not be cached: one with no usable
    /// modification time, one whose children were not all walked — a truncated
    /// node knows less than the walk that produced it — and one that came from
    /// the cache in the first place. Recording that last one would stamp it
    /// with the time of a measurement that never happened, and the subtree
    /// would be served past its own expiry for as long as scans kept coming.
    pub fn of(node: &DirNode, recorded_at: Timestamp) -> Option<Self> {
        if node.children_truncated || node.from_cache {
            return None;
        }
        Some(Self {
            key: CacheKey::of(&node.identity())?,
            size_bytes: node.size_bytes,
            allocated_bytes: node.allocated_bytes,
            file_count: node.file_count,
            dir_count: node.dir_count,
            dataless_count: node.dataless_count,
            largest_item_bytes: node.largest_item_bytes,
            has_hard_links: node.has_hard_links,
            has_truncation: node.has_truncation,
            recorded_at,
            child_dirs: Vec::new(),
            files: Vec::new(),
            kept_clones: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use jiff::Timestamp;

    use super::{CacheKey, DirRecord};
    use crate::scan::walker::{DirIdentity, DirNode};

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap_or_else(|e| panic!("{e}"))
    }

    fn identity(mtime: Option<Timestamp>) -> DirIdentity {
        DirIdentity { path: PathBuf::from("/vol/a"), device: 3, inode: 42, mtime }
    }

    fn node() -> DirNode {
        DirNode {
            path: PathBuf::from("/vol/a"),
            size_bytes: 500,
            allocated_bytes: 512,
            file_count: 4,
            dir_count: 1,
            dataless_count: 2,
            largest_item_bytes: 300,
            has_hard_links: false,
            has_truncation: false,
            device: 3,
            inode: 42,
            mtime: Some(at("2026-01-01T00:00:00Z")),
            children_truncated: false,
            from_cache: false,
        }
    }

    #[test]
    fn a_key_is_the_device_the_inode_and_the_modification_time() {
        let key = CacheKey::of(&identity(Some(at("1970-01-01T00:00:01Z"))));

        assert_eq!(key, Some(CacheKey { device: 3, inode: 42, mtime_ns: 1_000_000_000 }));
    }

    #[test]
    fn a_directory_without_a_usable_time_cannot_be_cached() {
        assert_eq!(CacheKey::of(&identity(None)), None);
    }

    #[test]
    fn a_touched_directory_gets_another_key() {
        let before = CacheKey::of(&identity(Some(at("2026-01-01T00:00:00Z"))));
        let after = CacheKey::of(&identity(Some(at("2026-01-01T00:00:01Z"))));

        assert_ne!(before, after);
    }

    #[test]
    fn a_record_carries_the_aggregate_of_the_node_it_was_made_from() {
        let recorded_at = at("2026-02-02T00:00:00Z");

        let record = DirRecord::of(&node(), recorded_at).unwrap_or_else(|| panic!("no record"));

        assert_eq!(record.size_bytes, 500);
        assert_eq!(record.allocated_bytes, 512);
        assert_eq!(record.file_count, 4);
        assert_eq!(record.dir_count, 1);
        assert_eq!(record.dataless_count, 2);
        assert_eq!(record.largest_item_bytes, 300);
        assert_eq!(record.recorded_at, recorded_at);
        assert_eq!(record.key.inode, 42);
    }

    #[test]
    fn a_truncated_node_is_never_recorded() {
        let truncated = DirNode { children_truncated: true, ..node() };

        assert!(DirRecord::of(&truncated, at("2026-02-02T00:00:00Z")).is_none());
    }

    #[test]
    fn a_node_without_a_time_is_never_recorded() {
        let timeless = DirNode { mtime: None, ..node() };

        assert!(DirRecord::of(&timeless, at("2026-02-02T00:00:00Z")).is_none());
    }
}
