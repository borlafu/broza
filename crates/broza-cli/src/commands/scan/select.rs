//! Narrowing an enumerated machine to what the flags asked for
//! (`docs/cli-spec.md` §3.1: `--volume`, `--no-external`).
//!
//! Two decisions are worth their comments. The order is `--no-external` first,
//! because `--no-external --volume "Time Machine"` asks for a volume that was
//! just excluded, and "not found" is a truer answer than quietly bringing the
//! external disk back. And a blank `--volume` is the only usage error here:
//! every other string is a name that might exist, so failing to find it is
//! exit `4`, not exit `2`.

use broza::BrozaError;
use broza::model::{Container, Disk};

use crate::args::ScanArgs;
use crate::commands::target::{is_blank, matches_id, matches_volume};

/// Apply `--no-external` and `--volume`, in that order.
///
/// # Errors
///
/// [`BrozaError::Usage`] (exit `2`) when `--volume` is blank, and
/// [`BrozaError::TargetNotFound`] (exit `4`) when it names nothing.
pub fn select(disks: Vec<Disk>, args: &ScanArgs) -> Result<Vec<Disk>, BrozaError> {
    let disks = if args.no_external { internal_only(disks) } else { disks };
    let Some(target) = args.volume.as_deref() else { return Ok(disks) };
    if is_blank(target) {
        return Err(BrozaError::Usage(
            "--volume needs a device id, a volume name or a mount point".to_owned(),
        ));
    }
    let selected = only(disks, target);
    if selected.is_empty() {
        return Err(BrozaError::TargetNotFound(format!(
            "no disk, container or volume named `{target}`; `broza scan` lists them all"
        )));
    }
    Ok(selected)
}

/// Drop every disk macOS does not report as internal.
fn internal_only(disks: Vec<Disk>) -> Vec<Disk> {
    disks.into_iter().filter(|disk| disk.internal).collect()
}

/// Keep only what `target` names, at whatever level it names it.
///
/// macOS puts disks, containers and volumes in one namespace, so `disk0`,
/// `disk3`, `disk3s5`, `Macintosh HD - Data` and `/System/Volumes/Data` are
/// all legitimate answers; each keeps the level it names and everything below
/// it. Disks and containers are matched by identifier only, because a name and
/// a mount point are things a volume has and they do not.
fn only(disks: Vec<Disk>, target: &str) -> Vec<Disk> {
    disks
        .into_iter()
        .filter_map(|disk| {
            if matches_id(&disk.id, target) {
                return Some(disk);
            }
            let containers = matching_containers(&disk, target);
            (!containers.is_empty()).then_some(Disk { containers, ..disk })
        })
        .collect()
}

/// The containers of `disk` that `target` names, directly or through a volume.
fn matching_containers(disk: &Disk, target: &str) -> Vec<Container> {
    disk.containers
        .iter()
        .filter_map(|container| {
            if matches_id(&container.id, target) {
                return Some(container.clone());
            }
            let volumes: Vec<_> =
                container.volumes.iter().filter(|volume| matches_volume(volume, target)).cloned().collect();
            (!volumes.is_empty()).then(|| Container { volumes, ..container.clone() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;

    use broza::ExitCode;
    use broza::model::{FsKind, Volume, VolumeId, VolumeRole};

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
            used_bytes: 1_000_000_000,
            writable_by_broza: role.writable_by_broza(),
            purpose: String::new(),
        }
    }

    fn container(raw: &str, volumes: Vec<Volume>) -> Container {
        Container {
            id: id(raw),
            kind: FsKind::Apfs,
            size_bytes: 10_000_000_000,
            used_bytes: 4_000_000_000,
            free_bytes: 6_000_000_000,
            purgeable_bytes: 1_000_000_000,
            volumes,
        }
    }

    fn machine() -> Vec<Disk> {
        vec![
            Disk {
                id: id("disk0"),
                model: "APPLE SSD".to_owned(),
                size_bytes: 1_000_000_000_000,
                internal: true,
                containers: vec![container(
                    "disk3",
                    vec![
                        volume("disk3s1", "Macintosh HD", VolumeRole::System, Some("/")),
                        volume("disk3s5", "Data", VolumeRole::Data, Some("/System/Volumes/Data")),
                    ],
                )],
            },
            Disk {
                id: id("disk4"),
                model: "Disk Image".to_owned(),
                size_bytes: 2_000_000_000,
                internal: false,
                containers: vec![container(
                    "disk4s1",
                    vec![volume("disk4s1", "Kiro CLI", VolumeRole::User, Some("/Volumes/Kiro CLI"))],
                )],
            },
        ]
    }

    pub(super) fn args() -> ScanArgs {
        ScanArgs {
            paths: Vec::new(),
            depth: crate::args::scan::DEFAULT_DEPTH,
            top: crate::args::scan::DEFAULT_TOP,
            min_size: None,
            volume: None,
            no_external: false,
            tree: false,
        }
    }

    fn with_volume(target: &str) -> ScanArgs {
        ScanArgs { volume: Some(target.to_owned()), ..args() }
    }

    #[test]
    fn without_flags_every_disk_survives() {
        assert_eq!(select(machine(), &args()).unwrap_or_else(|e| panic!("{e}")).len(), 2);
    }

    #[test]
    fn no_external_keeps_only_internal_disks() {
        let selected =
            select(machine(), &ScanArgs { no_external: true, ..args() }).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id.as_str(), "disk0");
    }

    #[test]
    fn a_volume_is_selected_by_id_by_name_or_by_mount_point() {
        for target in ["disk3s5", "Data", "/System/Volumes/Data"] {
            let selected =
                select(machine(), &with_volume(target)).unwrap_or_else(|e| panic!("{target}: {e}"));

            assert_eq!(selected.len(), 1, "{target}");
            assert_eq!(selected[0].containers.len(), 1, "{target}");
            let names: Vec<&str> =
                selected[0].containers[0].volumes.iter().map(|v| v.name.as_str()).collect();
            assert_eq!(names, vec!["Data"], "{target}");
        }
    }

    #[test]
    fn a_container_or_a_disk_may_be_named_by_its_identifier() {
        for (target, volumes) in [("disk3", 2), ("disk0", 2)] {
            let selected = select(machine(), &with_volume(target)).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(selected.len(), 1, "{target}");
            assert_eq!(selected[0].containers[0].volumes.len(), volumes, "{target}");
        }
    }

    #[test]
    fn a_volume_on_an_external_disk_is_selectable_by_name() {
        let selected = select(machine(), &with_volume("Kiro CLI")).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id.as_str(), "disk4");
    }

    #[test]
    fn a_blank_target_is_a_usage_error() {
        for blank in ["", "   "] {
            let error = select(machine(), &with_volume(blank)).expect_err("must fail");
            assert_eq!(ExitCode::from(&error), ExitCode::UsageError, "{blank:?}");
        }
    }

    #[test]
    fn a_well_formed_target_that_matches_nothing_is_not_found() {
        for target in ["disk9s9", "sda1", "No Such Volume", "/Volumes/Gone"] {
            let error = select(machine(), &with_volume(target)).expect_err("must fail");
            assert_eq!(ExitCode::from(&error), ExitCode::TargetNotFound, "{target}");
            assert!(error.to_string().contains(target), "{error}");
        }
    }

    #[test]
    fn excluding_externals_makes_an_external_volume_not_found() {
        let args = ScanArgs { no_external: true, ..with_volume("Kiro CLI") };

        let error = select(machine(), &args).expect_err("must fail");

        assert_eq!(ExitCode::from(&error), ExitCode::TargetNotFound);
    }
}
