//! Which volumes a scan covers (`docs/cli-spec.md` §3.1: `--volume`, `--no-external`).

use std::path::Path;

use crate::scan::mount::{MountEntry, MountTable};
use crate::scan::request::ScanRequest;

/// Where macOS mounts everything that is not the boot volume.
pub const EXTERNAL_MOUNT_PREFIX: &str = "/Volumes";

/// Volumes a scan covers, ordered by volume id.
///
/// Read-only volumes (`system`, `preboot`, `recovery`, `vm`) are enumerated by
/// `broza scan` but not walked: their contents are sealed and Broza can do
/// nothing about them (`AGENTS.md` §2.3).
pub fn selected_volumes<'a>(mounts: &'a MountTable, request: &ScanRequest) -> Vec<&'a MountEntry> {
    let mut selected: Vec<&MountEntry> = mounts
        .entries()
        .iter()
        .filter(|entry| entry.volume.writable_by_broza)
        .filter(|entry| request.include_external || !is_external(&entry.mount_point))
        .filter(|entry| request.volume.as_deref().is_none_or(|wanted| matches(entry, wanted)))
        .collect();
    selected.sort_by(|left, right| left.volume.id.cmp(&right.volume.id));
    selected
}

/// `true` when `mount_point` is under `/Volumes`, which is what "external" means.
pub fn is_external(mount_point: &Path) -> bool {
    mount_point.starts_with(EXTERNAL_MOUNT_PREFIX)
}

/// `true` when `wanted` names this volume: its id, its name, or its mount point.
pub fn matches(entry: &MountEntry, wanted: &str) -> bool {
    entry.volume.id.as_str() == wanted
        || entry.volume.name.eq_ignore_ascii_case(wanted)
        || entry.mount_point == Path::new(wanted)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{is_external, matches, selected_volumes};
    use crate::model::{Volume, VolumeId, VolumeRole};
    use crate::scan::mount::{MountEntry, MountTable};
    use crate::scan::request::ScanRequest;

    fn entry(id: &str, name: &str, role: VolumeRole, mount: &str) -> MountEntry {
        MountEntry {
            mount_point: PathBuf::from(mount),
            device: 1,
            volume: Volume {
                id: id.parse::<VolumeId>().unwrap_or_else(|e| panic!("{e}")),
                name: name.to_owned(),
                role,
                mount_point: Some(PathBuf::from(mount)),
                used_bytes: 0,
                writable_by_broza: role.writable_by_broza(),
                purpose: String::new(),
            },
            firmlinks: Vec::new(),
        }
    }

    fn table() -> MountTable {
        MountTable::new(vec![
            entry("disk3s5", "Macintosh HD - Data", VolumeRole::Data, "/System/Volumes/Data"),
            entry("disk3s1", "Macintosh HD", VolumeRole::System, "/"),
            entry("disk4s1", "Backup Drive", VolumeRole::User, "/Volumes/Backup Drive"),
        ])
    }

    fn ids(entries: &[&MountEntry]) -> Vec<String> {
        entries.iter().map(|entry| entry.volume.id.to_string()).collect()
    }

    #[test]
    fn read_only_volumes_are_never_walked() {
        let table = table();

        let selected = selected_volumes(&table, &ScanRequest::default());

        assert_eq!(ids(&selected), vec!["disk3s5".to_owned(), "disk4s1".to_owned()]);
    }

    #[test]
    fn volumes_come_back_ordered_by_identifier() {
        let table = MountTable::new(vec![
            entry("disk9s1", "Zed", VolumeRole::User, "/Volumes/Zed"),
            entry("disk3s5", "Data", VolumeRole::Data, "/System/Volumes/Data"),
        ]);

        assert_eq!(
            ids(&selected_volumes(&table, &ScanRequest::default())),
            vec!["disk3s5".to_owned(), "disk9s1".to_owned()]
        );
    }

    #[test]
    fn no_external_drops_what_is_mounted_under_volumes() {
        let request = ScanRequest { include_external: false, ..ScanRequest::default() };

        assert_eq!(ids(&selected_volumes(&table(), &request)), vec!["disk3s5".to_owned()]);
    }

    #[test]
    fn a_volume_can_be_named_by_id_by_name_or_by_mount_point() {
        for wanted in ["disk4s1", "Backup Drive", "backup drive", "/Volumes/Backup Drive"] {
            let request = ScanRequest::default().for_volume(wanted);

            assert_eq!(ids(&selected_volumes(&table(), &request)), vec!["disk4s1".to_owned()], "{wanted}");
        }
    }

    #[test]
    fn a_volume_nobody_has_selects_nothing() {
        let request = ScanRequest::default().for_volume("disk99s9");

        assert!(selected_volumes(&table(), &request).is_empty());
    }

    #[test]
    fn a_read_only_volume_stays_out_even_when_it_is_asked_for_by_name() {
        let request = ScanRequest::default().for_volume("disk3s1");

        assert!(selected_volumes(&table(), &request).is_empty());
    }

    #[test]
    fn only_mounts_under_volumes_count_as_external() {
        assert!(is_external(&PathBuf::from("/Volumes/Backup")));
        assert!(!is_external(&PathBuf::from("/System/Volumes/Data")));
        assert!(!is_external(&PathBuf::from("/")));
    }

    #[test]
    fn a_selector_that_matches_nothing_of_the_entry_does_not_match() {
        let data = entry("disk3s5", "Data", VolumeRole::Data, "/System/Volumes/Data");

        assert!(matches(&data, "disk3s5"));
        assert!(!matches(&data, "disk3s"));
        assert!(!matches(&data, "/System/Volumes"));
    }
}
