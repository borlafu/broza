//! A machine to assemble, built in a test rather than read from a disk.
//!
//! Owns everything [`Inputs`] borrows, so a test can describe the situation it
//! cares about in a few builder calls and hand the bundle straight to the
//! assembly.

use std::path::Path;

use super::inputs::{InfoByDevice, Inputs};
use super::plist_apfs::ApfsList;
use super::plist_info::DeviceInfo;
use super::plist_list::DiskList;
use super::roles::TIME_MACHINE_MARKER;
use crate::adapters::tmutil_destinations::{BackupDestinations, parse_destination_info};
use crate::testing::{FakeFileOps, FakeSpace};

/// The parsed output and fake ports of one imagined machine.
#[derive(Debug, Default)]
pub(crate) struct Scenario {
    /// What `diskutil list` would have said.
    pub list: DiskList,
    /// What `diskutil apfs list` would have said.
    pub apfs: ApfsList,
    /// What `diskutil info` would have said, per device.
    pub infos: InfoByDevice,
    /// What `tmutil destinationinfo -X` would have said.
    pub destinations: BackupDestinations,
    /// Purgeable estimates per mount point.
    pub space: FakeSpace,
    /// The filesystem the Time Machine marker is looked for in.
    pub fs: FakeFileOps,
}

impl Scenario {
    /// A machine with no disks, no purgeable space and an empty filesystem.
    pub fn new() -> Self {
        Self::default()
    }

    /// Use `list` as the partition map.
    #[must_use]
    pub fn with_list(mut self, list: DiskList) -> Self {
        self.list = list;
        self
    }

    /// Use `apfs` as the container listing.
    #[must_use]
    pub fn with_apfs(mut self, apfs: ApfsList) -> Self {
        self.apfs = apfs;
        self
    }

    /// Answer `diskutil info` with `infos`, keyed by their own identifier.
    #[must_use]
    pub fn with_infos(mut self, infos: Vec<DeviceInfo>) -> Self {
        self.infos = infos.into_iter().map(|info| (info.device_identifier.clone(), info)).collect();
        self
    }

    /// Report `bytes` as purgeable for `mount_point`.
    #[must_use]
    pub fn with_purgeable(self, mount_point: &str, bytes: u64) -> Self {
        self.space.set_purgeable(mount_point, bytes);
        self
    }

    /// Let Time Machine claim the destinations in `destination_info`, which is
    /// the output `tmutil destinationinfo -X` would have printed.
    #[must_use]
    pub fn with_destination_info(mut self, destination_info: &[u8]) -> Self {
        self.destinations =
            parse_destination_info(destination_info).unwrap_or_else(|error| panic!("{error}"));
        self
    }

    /// Put a Time Machine backup directory at the root of `mount_point`.
    #[must_use]
    pub fn with_backup_marker(self, mount_point: &str) -> Self {
        self.fs.add_dir(Path::new(mount_point).join(TIME_MACHINE_MARKER));
        self
    }

    /// Borrow everything as the assembly expects it.
    pub fn inputs(&self) -> Inputs<'_> {
        Inputs {
            list: &self.list,
            apfs: &self.apfs,
            infos: &self.infos,
            destinations: &self.destinations,
            space: &self.space,
            fs: &self.fs,
        }
    }
}
