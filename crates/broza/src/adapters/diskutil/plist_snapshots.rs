//! `diskutil apfs listSnapshots -plist <volume>`: the local snapshots of a volume.
//!
//! macOS reports a name, a UUID and whether the snapshot is purgeable, but never
//! a size (`docs/adr/0002-diskutil-plist-over-diskarbitration.md`), which is why
//! [`Snapshot`] has no byte count to fill in.

use serde::Deserialize;

use super::parse::{optional_text, parse_plist};
use crate::BrozaError;
use crate::model::Snapshot;

/// Command this module parses, for error messages.
const COMMAND: &str = "diskutil apfs listSnapshots -plist";

/// The whole output of `diskutil apfs listSnapshots -plist`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct SnapshotList {
    /// Snapshots of the volume, in the order macOS lists them.
    snapshots: Vec<SnapshotEntry>,
}

/// One snapshot entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct SnapshotEntry {
    /// Snapshot name (`com.apple.TimeMachine.2026-09-20-101530.local`).
    #[serde(deserialize_with = "optional_text")]
    snapshot_name: Option<String>,
    /// UUID of the snapshot.
    #[serde(rename = "SnapshotUUID", deserialize_with = "optional_text")]
    snapshot_uuid: Option<String>,
    /// `true` when macOS considers the snapshot reclaimable on its own.
    purgeable: bool,
}

impl SnapshotEntry {
    /// The contract type, keeping the name verbatim.
    ///
    /// A snapshot without a name cannot be acted on later, so it is reported as
    /// the empty string rather than dropped: hiding it would make Broza's
    /// snapshot count disagree with `diskutil`'s.
    fn into_model(self) -> Snapshot {
        Snapshot {
            name: self.snapshot_name.unwrap_or_default(),
            uuid: self.snapshot_uuid,
            purgeable: self.purgeable,
        }
    }
}

/// Parse the output of `diskutil apfs listSnapshots -plist <volume>`.
pub fn parse_snapshots(bytes: &[u8]) -> Result<Vec<Snapshot>, BrozaError> {
    let parsed: SnapshotList = parse_plist(bytes, COMMAND)?;
    Ok(parsed.snapshots.into_iter().map(SnapshotEntry::into_model).collect())
}

#[cfg(test)]
mod tests {
    use super::parse_snapshots;

    /// One purgeable Time Machine snapshot and one nameless entry. macOS 26 on
    /// the development machine only ever produced `com.apple.os.update-*`
    /// snapshots, so the purgeable case is written out here rather than
    /// recorded (`AGENTS.md` §11).
    const SAMPLE: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>Snapshots</key>
  <array>
    <dict>
      <key>Purgeable</key><true/>
      <key>SnapshotName</key><string>com.apple.TimeMachine.2026-09-20-101530.local</string>
      <key>SnapshotUUID</key><string>00000001-1111-4222-8333-000000000001</string>
    </dict>
    <dict>
      <key>Purgeable</key><false/>
      <key>SnapshotName</key><string></string>
    </dict>
  </array>
</dict>
</plist>"#;

    #[test]
    fn a_purgeable_snapshot_keeps_its_name_uuid_and_flag() {
        let parsed = parse_snapshots(SAMPLE).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "com.apple.TimeMachine.2026-09-20-101530.local");
        assert_eq!(parsed[0].uuid.as_deref(), Some("00000001-1111-4222-8333-000000000001"));
        assert!(parsed[0].purgeable);
    }

    #[test]
    fn a_nameless_snapshot_is_still_counted() {
        let parsed = parse_snapshots(SAMPLE).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(parsed[1].name, "");
        assert_eq!(parsed[1].uuid, None);
        assert!(!parsed[1].purgeable);
    }

    #[test]
    fn a_volume_with_no_snapshots_parses_into_an_empty_list() {
        let empty = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Snapshots</key><array/></dict></plist>"#;

        assert!(parse_snapshots(empty).unwrap_or_else(|e| panic!("{e}")).is_empty());
    }

    #[test]
    fn output_that_is_not_a_plist_is_an_error_and_not_a_panic() {
        assert!(parse_snapshots(b"<<<").is_err());
    }
}
