//! Building one container and its volumes, and deciding what may be written to.
//!
//! Every volume that reaches the JSON contract passes through here, so this is
//! where "is this the user's disk?" is answered. It is answered by
//! [`super::roles::volume_role`] from facts Broza actually observed — the roles
//! `diskutil apfs list` reports, `WritableVolume` from `diskutil info`, the
//! volume's own mount point and a Time Machine marker on disk — and never by
//! the filesystem family alone (`AGENTS.md` §2.3).

use std::path::Path;

use super::devices::ordered_by_device;
use super::inputs::{Inputs, warning};
use super::plist_apfs::{ApfsContainer, ApfsVolume};
use super::plist_info::DeviceInfo;
use super::plist_list::ListApfsVolume;
use super::purpose::purpose_for_volume;
use super::roles::{BackupEvidence, TIME_MACHINE_MARKER, VolumeFacts, volume_role};
use crate::model::{Container, FsKind, Volume, VolumeId, VolumeRole, Warning};

/// Purgeable bytes reported when macOS will not answer.
pub(super) const UNKNOWN_PURGEABLE_BYTES: u64 = 0;
/// Warning code for a device `diskutil` named in a way Broza cannot parse.
pub(crate) const UNREADABLE_ID_CODE: &str = "unreadable_device_id";
/// Warning code for a data volume macOS mounted read-only.
pub(crate) const DATA_NOT_WRITABLE_CODE: &str = "data_volume_not_writable";

/// One APFS container with its volumes, or `None` when it cannot be identified.
pub(crate) fn apfs_container(
    container: &ApfsContainer,
    inputs: &Inputs<'_>,
    warnings: &mut Vec<Warning>,
) -> Option<Container> {
    let Ok(id) = container.container_reference.parse::<VolumeId>() else {
        warnings.push(warning(
            UNREADABLE_ID_CODE,
            format!(
                "skipped an APFS container: `{}` is not a BSD device name",
                container.container_reference
            ),
            None,
        ));
        return None;
    };
    let volumes = ordered_by_device(
        container.volumes.iter().filter_map(|volume| apfs_volume(volume, inputs, warnings)).collect(),
        |volume| volume.id.as_str(),
    );
    Some(Container {
        id,
        kind: FsKind::Apfs,
        size_bytes: container.capacity_ceiling,
        used_bytes: container.used_bytes(),
        free_bytes: container.capacity_free,
        purgeable_bytes: purgeable_of(&volumes, inputs),
        volumes,
    })
}

/// One APFS volume of a container.
fn apfs_volume(volume: &ApfsVolume, inputs: &Inputs<'_>, warnings: &mut Vec<Warning>) -> Option<Volume> {
    let Ok(id) = volume.device_identifier.parse::<VolumeId>() else {
        warnings.push(warning(
            UNREADABLE_ID_CODE,
            format!("skipped a volume: `{}` is not a BSD device name", volume.device_identifier),
            None,
        ));
        return None;
    };
    let listed = inputs.list.apfs_volume(&volume.device_identifier);
    let name = volume
        .name
        .clone()
        .or_else(|| listed.and_then(|listed| listed.volume_name.clone()))
        .unwrap_or_default();
    // The volume's own mount point decides the role; the snapshot standing in
    // for a sealed system volume decides only what Broza reports.
    let own_mount_point = listed.and_then(|listed| listed.mount_point.clone());
    let info = inputs.infos.get(&volume.device_identifier);
    let observed = facts(info, &name, volume.apfs_volume_uuid.as_deref(), own_mount_point.as_deref(), inputs);
    let role = volume_role(&volume.roles, own_mount_point.as_deref(), observed);
    Some(Volume {
        id,
        purpose: purpose_for_volume(role, &volume.roles, &name),
        writable_by_broza: writable(role, observed, &name, warnings),
        name,
        role,
        uuid: volume.apfs_volume_uuid.clone().or_else(|| info.and_then(|info| info.volume_uuid.clone())),
        mount_point: listed.and_then(ListApfsVolume::effective_mount_point),
        used_bytes: volume.capacity_in_use,
    })
}

/// Whether Broza may write to a volume: its role must allow it *and* macOS
/// must agree that the volume is writable.
///
/// The role is the first gate and no flag can open it (`AGENTS.md` §2.3). The
/// second gate is `WritableVolume`: a data volume mounted read-only — by
/// `FileVault` before unlock, by a failing disk macOS remounted read-only, by a
/// recovery boot — is not somewhere Broza can move files to, and planning a
/// cleanup for it would only produce failures at apply time. The disagreement
/// is worth saying out loud, so it becomes a warning.
pub(super) fn writable(
    role: VolumeRole,
    facts: VolumeFacts<'_>,
    name: &str,
    warnings: &mut Vec<Warning>,
) -> bool {
    if !role.writable_by_broza() {
        return false;
    }
    if facts.writable_volume {
        return true;
    }
    if role == VolumeRole::Data {
        warnings.push(warning(
            DATA_NOT_WRITABLE_CODE,
            format!(
                "the data volume {} is mounted read-only, so Broza cannot clean anything on it",
                display_name(name)
            ),
            None,
        ));
    }
    false
}

/// How a warning refers to a volume that may have no name.
fn display_name(name: &str) -> &str {
    if name.is_empty() { "(unnamed)" } else { name }
}

/// Everything Broza observed about a volume besides its declared roles.
pub(super) fn facts<'a>(
    info: Option<&'a DeviceInfo>,
    name: &'a str,
    uuid: Option<&str>,
    mount_point: Option<&Path>,
    inputs: &Inputs<'_>,
) -> VolumeFacts<'a> {
    VolumeFacts {
        writable_volume: info.is_some_and(|info| info.writable_volume),
        backup_evidence: backup_evidence(name, uuid, mount_point, inputs),
        time_machine_marker: mount_point
            .is_some_and(|mount_point| inputs.fs.exists(&mount_point.join(TIME_MACHINE_MARKER))),
        content: info.and_then(|info| info.content.as_deref()),
        name: Some(name),
    }
}

/// How firmly Time Machine claims this volume.
///
/// Being the destination — by mount point or by identifier — is a fact about
/// the volume; sharing a destination's name is not, and only counts where the
/// alternative reading would be "the user's disk".
fn backup_evidence(
    name: &str,
    uuid: Option<&str>,
    mount_point: Option<&Path>,
    inputs: &Inputs<'_>,
) -> BackupEvidence {
    if inputs.destinations.contains(mount_point, uuid) {
        return BackupEvidence::Volume;
    }
    if inputs.destinations.contains_name(name) {
        return BackupEvidence::Name;
    }
    BackupEvidence::None
}

/// Purgeable bytes of a container: the largest estimate its own volumes report.
///
/// The question is asked of every mounted volume Broza considers the user's —
/// role `data` or `user` — because a container can hold more than one. They all
/// share the same free space, so the answers describe the same pool and the
/// largest is the estimate for the container; summing them would count the same
/// bytes twice. A container with no such volume, or a mount point macOS
/// refuses to answer for, reports zero: an estimate nobody made is not a guess
/// Broza invents (`AGENTS.md` §2.7).
pub(super) fn purgeable_of(volumes: &[Volume], inputs: &Inputs<'_>) -> u64 {
    volumes
        .iter()
        .filter(|volume| matches!(volume.role, VolumeRole::Data | VolumeRole::User))
        .filter_map(|volume| volume.mount_point.as_ref())
        .filter_map(|mount_point| inputs.space.purgeable_bytes(mount_point).ok())
        .max()
        .unwrap_or(UNKNOWN_PURGEABLE_BYTES)
}
