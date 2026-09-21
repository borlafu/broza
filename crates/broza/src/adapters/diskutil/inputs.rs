//! Everything the assembly reads, gathered in one place.
//!
//! Three `diskutil` outputs plus the two ports that answer questions
//! `diskutil` cannot: purgeable space, and whether a Time Machine marker sits
//! at a volume root. Passing them as one borrowed bundle keeps the assembly
//! functions short and makes the whole of it testable without a process.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::plist_apfs::ApfsList;
use super::plist_info::DeviceInfo;
use super::plist_list::DiskList;
use crate::model::Warning;
use crate::ports::{FileOps, SpaceProvider};

/// `diskutil info` output, keyed by BSD identifier.
pub(crate) type InfoByDevice = BTreeMap<String, DeviceInfo>;

/// The parsed output and the ports one assembly run needs.
pub(crate) struct Inputs<'a> {
    /// Which devices exist and where volumes are mounted.
    pub list: &'a DiskList,
    /// Container capacities, volume roles and bytes in use.
    pub apfs: &'a ApfsList,
    /// Per-device details: model, writability, free space.
    pub infos: &'a InfoByDevice,
    /// The purgeable estimate of a mount point.
    pub space: &'a dyn SpaceProvider,
    /// Used only to look for a Time Machine marker at a volume root.
    pub fs: &'a dyn FileOps,
}

/// Build one entry of `warnings[]`.
pub(crate) fn warning(code: &str, message: String, path: Option<PathBuf>) -> Warning {
    Warning { code: code.to_owned(), message, path }
}
