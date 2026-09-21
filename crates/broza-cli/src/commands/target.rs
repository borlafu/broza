//! Naming a volume on the command line, shared by `scan --volume` and `explain`.
//!
//! `docs/cli-spec.md` §3.1 and §3.2 both say "device id, name or mount point",
//! and they have to mean the same thing: a user who found a volume with
//! `broza explain "Macintosh HD - Data"` must be able to paste that into
//! `broza scan --volume`. One matcher, used by both, is how that stays true.
//!
//! Matching is exact, never fuzzy. A prefix match would be friendlier and
//! would also, one day, quietly select the wrong volume.

use std::path::Path;

use broza::model::{Disk, Volume, VolumeId};

/// `true` when `target` names `volume` by id, by Finder name, or by mount point.
pub fn matches_volume(volume: &Volume, target: &str) -> bool {
    volume.id.as_str() == target
        || volume.name == target
        || volume.mount_point.as_deref() == Some(Path::new(target))
}

/// `true` when `target` is the BSD identifier of a disk or a container.
///
/// A disk and a container have no name and no mount point of their own, so
/// their identifier is the only way to name them.
pub fn matches_id(id: &VolumeId, target: &str) -> bool {
    id.as_str() == target
}

/// The first volume of `disks` that `target` names, if any.
pub fn volume_named(disks: &[Disk], target: &str) -> Option<Volume> {
    disks
        .iter()
        .flat_map(|disk| &disk.containers)
        .flat_map(|container| &container.volumes)
        .find(|volume| matches_volume(volume, target))
        .cloned()
}

/// `true` when `target` cannot name anything at all.
///
/// An empty or blank `--volume` is a usage error (exit `2`); any other string
/// is a legitimate name that simply may not exist (exit `4`).
pub fn is_blank(target: &str) -> bool {
    target.trim().is_empty()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;

    use broza::model::{Container, FsKind, VolumeRole};

    use super::*;

    fn id(raw: &str) -> VolumeId {
        raw.parse().unwrap_or_else(|e| panic!("{raw}: {e}"))
    }

    fn volume(raw: &str, name: &str, role: VolumeRole, mount: Option<&str>) -> Volume {
        Volume {
            id: id(raw),
            name: name.to_owned(),
            uuid: None,
            role,
            mount_point: mount.map(PathBuf::from),
            used_bytes: 1,
            writable_by_broza: role.writable_by_broza(),
            purpose: String::new(),
        }
    }

    fn disks() -> Vec<Disk> {
        vec![Disk {
            id: id("disk0"),
            model: "APPLE SSD".to_owned(),
            size_bytes: 1,
            internal: true,
            containers: vec![Container {
                id: id("disk3"),
                kind: FsKind::Apfs,
                size_bytes: 1,
                used_bytes: 1,
                free_bytes: 0,
                purgeable_bytes: 0,
                volumes: vec![
                    volume("disk3s1", "Macintosh HD", VolumeRole::System, Some("/")),
                    volume("disk3s5", "Data", VolumeRole::Data, Some("/System/Volumes/Data")),
                    volume("disk3s2", "Preboot", VolumeRole::Preboot, None),
                ],
            }],
        }]
    }

    #[test]
    fn all_three_forms_name_the_same_volume() {
        for target in ["disk3s5", "Data", "/System/Volumes/Data"] {
            let found = volume_named(&disks(), target);
            assert_eq!(found.map(|v| v.id.to_string()), Some("disk3s5".to_owned()), "{target}");
        }
    }

    #[test]
    fn an_unmounted_volume_is_still_named_by_id_or_by_name() {
        assert!(volume_named(&disks(), "disk3s2").is_some());
        assert!(volume_named(&disks(), "Preboot").is_some());
    }

    #[test]
    fn matching_is_exact_and_never_a_prefix() {
        for target in ["disk3s", "disk3s55", "Dat", "Data ", "/System/Volumes"] {
            assert!(volume_named(&disks(), target).is_none(), "{target:?} must not match");
        }
    }

    #[test]
    fn a_disk_or_a_container_is_named_only_by_its_identifier() {
        assert!(matches_id(&id("disk3"), "disk3"));
        assert!(!matches_id(&id("disk3"), "disk3s5"));
        assert!(!matches_id(&id("disk3"), "Macintosh HD"));
    }

    #[test]
    fn a_blank_target_names_nothing_and_is_recognised_as_such() {
        for blank in ["", " ", "\t", "\n"] {
            assert!(is_blank(blank), "{blank:?}");
        }
        for named in ["disk3s5", "Data", " Data"] {
            assert!(!is_blank(named), "{named:?}");
        }
    }

    #[test]
    fn a_volume_is_matched_by_its_own_mount_point_and_not_by_a_child_path() {
        let data = volume("disk3s5", "Data", VolumeRole::Data, Some("/System/Volumes/Data"));
        assert!(matches_volume(&data, "/System/Volumes/Data"));
        assert!(!matches_volume(&data, "/System/Volumes/Data/Users"));
    }
}
