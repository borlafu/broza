//! What the cache is keyed by, and what it stores.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::scan::walker::{DirIdentity, DirNode};

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
    /// descendant directory's subtree.
    ///
    /// This is what lets a warm scan stay honest. A subtree may only be served
    /// from the cache when nothing inside it could have been *listed* on its
    /// own, and that is decided by comparing this against `--min-size`.
    pub largest_item_bytes: u64,
    /// When the aggregate was measured; the TTL is counted from here.
    pub recorded_at: Timestamp,
}

impl DirRecord {
    /// Record of a freshly walked directory.
    ///
    /// `None` for a node that must not be cached: one with no usable modification
    /// time, and one whose children were not all walked — a truncated node knows
    /// less than the walk that produced it, and caching it would freeze that gap.
    pub fn of(node: &DirNode, recorded_at: Timestamp) -> Option<Self> {
        if node.children_truncated {
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
            recorded_at,
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
            device: 3,
            inode: 42,
            mtime: Some(at("2026-01-01T00:00:00Z")),
            children_truncated: false,
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
