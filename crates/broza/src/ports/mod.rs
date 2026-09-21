//! Ports: the traits through which the core reaches the outside world.
//!
//! Traits only. Real macOS implementations live in `adapters/`; fakes for tests live in
//! `testing/`. The core never spawns processes, reads the environment, or touches the
//! filesystem except through these traits.

pub mod clock;
pub mod disk_enum;
pub mod fs_ops;
pub mod process;
pub mod prompter;

use std::sync::Arc;

pub use clock::Clock;
pub use disk_enum::{DiskEnumerator, EnumerationReport, SnapshotProvider, SpaceProvider};
pub use fs_ops::{DirListing, EntryMetadata, FileOps};
pub use process::{ProcessOutput, ProcessRunner};
pub use prompter::{Answer, ConfirmationRequest, Prompter};

/// Dependency bundle handed to the engine. Cheap to clone.
#[derive(Clone)]
pub struct Ports {
    /// Runs external commands (`diskutil`, `tmutil`, `xcrun`).
    pub process: Arc<dyn ProcessRunner>,
    /// Enumerates disks, containers, and volumes.
    pub disks: Arc<dyn DiskEnumerator>,
    /// Reports purgeable space per mount point.
    pub space: Arc<dyn SpaceProvider>,
    /// Lists and deletes APFS local snapshots.
    pub snapshots: Arc<dyn SnapshotProvider>,
    /// Filesystem reads and writes.
    pub fs: Arc<dyn FileOps>,
    /// Current time.
    pub clock: Arc<dyn Clock>,
    /// Interactive confirmation.
    pub prompter: Arc<dyn Prompter>,
}

impl std::fmt::Debug for Ports {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ports { .. }")
    }
}
