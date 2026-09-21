//! `scan` payload (`docs/cli-spec.md` §4.2).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::disk::Disk;
use crate::model::ids::VolumeId;

/// Payload of `broza scan`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ScanReport {
    /// Every disk Broza could enumerate.
    #[serde(default)]
    pub disks: Vec<Disk>,
    /// Biggest items found while walking the mounted volumes, largest first.
    #[serde(default)]
    pub largest_items: Vec<LargestItem>,
}

/// One entry of the "largest items" table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LargestItem {
    /// Absolute path of the item.
    pub path: PathBuf,
    /// Size in bytes. Hard links and APFS clones are counted once.
    pub size_bytes: u64,
    /// Whether the entry is a file or a directory.
    pub kind: ItemKind,
    /// Volume the item lives on.
    pub volume_id: VolumeId,
}

/// Kind of a [`LargestItem`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ItemKind {
    /// A regular file.
    File,
    /// A directory, including application and document bundles.
    Directory,
}

#[cfg(test)]
mod tests {
    use super::{ItemKind, LargestItem, ScanReport};

    #[test]
    fn an_empty_report_serializes_to_empty_arrays() {
        let json = serde_json::to_value(ScanReport::default()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(json, serde_json::json!({"disks": [], "largest_items": []}));
    }

    #[test]
    fn item_kinds_use_the_identifiers_of_the_specification() {
        for (kind, expected) in [(ItemKind::File, "\"file\""), (ItemKind::Directory, "\"directory\"")] {
            assert_eq!(serde_json::to_string(&kind).unwrap_or_else(|e| panic!("{e}")), expected);
        }
        assert!(serde_json::from_str::<ItemKind>("\"socket\"").is_err());
    }

    #[test]
    fn a_report_carrying_a_disk_round_trips() {
        let raw = serde_json::json!({
            "disks": [{
                "id": "disk0",
                "model": "APPLE SSD AP1024Z",
                "size_bytes": 1_000_555_581_440_u64,
                "internal": true,
                "containers": []
            }],
            "largest_items": []
        });
        let report: ScanReport = serde_json::from_value(raw.clone()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(report.disks.len(), 1);
        assert_eq!(serde_json::to_value(&report).unwrap_or_else(|e| panic!("{e}")), raw);
    }

    #[test]
    fn a_malformed_volume_identifier_is_rejected() {
        let raw = serde_json::json!({
            "disks": [],
            "largest_items": [{"path": "/x", "size_bytes": 1, "kind": "file", "volume_id": "sda1"}]
        });
        assert!(serde_json::from_value::<ScanReport>(raw).is_err());
    }

    #[test]
    fn a_largest_item_round_trips() {
        let item = LargestItem {
            path: "/Users/x/Library/Developer".into(),
            size_bytes: 312_400_000_000,
            kind: ItemKind::Directory,
            volume_id: "disk3s5".parse().unwrap_or_else(|e| panic!("{e}")),
        };
        let json = serde_json::to_value(&item).unwrap_or_else(|e| panic!("{e}"));
        let back: LargestItem = serde_json::from_value(json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back, item);
    }
}
