//! Mount table: resolves a path to the volume it lives on, firmlink-aware.
//!
//! On a modern Mac `/Users` lives on the Data volume although `/` is the System volume.
//! `statfs` on a firmlinked path already reports the Data mount point
//! (`/System/Volumes/Data`), so the table stores both the canonical mount point and
//! the firmlink prefixes that redirect to it.

use std::path::{Path, PathBuf};

use crate::model::{Volume, VolumeRole};

/// A mounted volume with its device id and any firmlink prefixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    /// Where the volume is mounted (`/System/Volumes/Data`).
    pub mount_point: PathBuf,
    /// Device id as seen in `st_dev`.
    pub device: u64,
    /// Volume this mount belongs to.
    pub volume: Volume,
    /// Additional prefixes that resolve to this mount (`/Users`, `/Applications`).
    pub firmlinks: Vec<PathBuf>,
}

/// Immutable lookup from path to mounted volume.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MountTable {
    entries: Vec<MountEntry>,
}

impl MountTable {
    /// Build a table; longest-prefix matching decides ties.
    pub fn new(entries: Vec<MountEntry>) -> Self {
        Self { entries }
    }

    /// All entries.
    pub fn entries(&self) -> &[MountEntry] {
        &self.entries
    }

    /// Volume that contains `path`, by longest matching mount point or firmlink.
    pub fn volume_for(&self, path: &Path) -> Option<&MountEntry> {
        self.entries
            .iter()
            .filter_map(|entry| Self::match_len(entry, path).map(|len| (len, entry)))
            .max_by_key(|(len, _)| *len)
            .map(|(_, entry)| entry)
    }

    /// Role of the volume containing `path`; `None` when unknown.
    pub fn role_for(&self, path: &Path) -> Option<VolumeRole> {
        self.volume_for(path).map(|entry| entry.volume.role)
    }

    /// Entry whose device id equals `device`.
    pub fn by_device(&self, device: u64) -> Option<&MountEntry> {
        self.entries.iter().find(|entry| entry.device == device)
    }

    fn match_len(entry: &MountEntry, path: &Path) -> Option<usize> {
        std::iter::once(&entry.mount_point)
            .chain(entry.firmlinks.iter())
            .filter(|prefix| path.starts_with(prefix))
            .map(|prefix| prefix.components().count())
            .max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::VolumeId;

    fn volume(id: &str, role: VolumeRole, mount: &str) -> Volume {
        Volume {
            id: id.parse::<VolumeId>().unwrap_or_else(|e| panic!("{e}")),
            name: id.to_owned(),
            role,
            mount_point: Some(PathBuf::from(mount)),
            used_bytes: 0,
            writable_by_broza: role.writable_by_broza(),
            purpose: String::new(),
        }
    }

    fn table() -> MountTable {
        MountTable::new(vec![
            MountEntry {
                mount_point: PathBuf::from("/"),
                device: 1,
                volume: volume("disk3s1", VolumeRole::System, "/"),
                firmlinks: vec![],
            },
            MountEntry {
                mount_point: PathBuf::from("/System/Volumes/Data"),
                device: 2,
                volume: volume("disk3s5", VolumeRole::Data, "/System/Volumes/Data"),
                firmlinks: vec![PathBuf::from("/Users"), PathBuf::from("/Applications")],
            },
            MountEntry {
                mount_point: PathBuf::from("/Volumes/External"),
                device: 3,
                volume: volume("disk4s1", VolumeRole::User, "/Volumes/External"),
                firmlinks: vec![],
            },
        ])
    }

    #[test]
    fn resolves_firmlinked_home_to_data_volume() {
        let t = table();
        assert_eq!(t.role_for(Path::new("/Users/dana/Library/Caches")), Some(VolumeRole::Data));
        assert_eq!(t.role_for(Path::new("/System/Volumes/Data/Users/dana")), Some(VolumeRole::Data));
    }

    #[test]
    fn root_paths_resolve_to_system_volume() {
        assert_eq!(table().role_for(Path::new("/System/Library")), Some(VolumeRole::System));
        assert_eq!(table().role_for(Path::new("/usr/bin")), Some(VolumeRole::System));
    }

    #[test]
    fn longest_prefix_wins_for_external_volume() {
        let t = table();
        let entry = t.volume_for(Path::new("/Volumes/External/Movies")).map(|e| e.device);
        assert_eq!(entry, Some(3));
    }

    #[test]
    fn relative_paths_do_not_resolve() {
        assert!(table().volume_for(Path::new("Users/dana")).is_none());
    }

    #[test]
    fn lookup_by_device() {
        assert_eq!(table().by_device(2).map(|e| e.volume.role), Some(VolumeRole::Data));
        assert!(table().by_device(99).is_none());
    }
}
