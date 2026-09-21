//! One fake machine for the command tests: a Data volume, a home with a cache
//! file and the Docker disk image, and the contexts the commands take.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use broza::config::Config;
use broza::model::{Container, Disk, FsKind, Host, Volume, VolumeId, VolumeRole};
use broza::ports::{Ports, Prompter};
use broza::testing::{FakePrompter, Handles};
use jiff::Timestamp;

use crate::commands::scan::folders::FolderSettings;
use crate::commands::store::StoreContext;
use crate::output::ColorPolicy;

/// The home of the fake machine, in the Data-volume spelling.
pub const HOME: &str = "/System/Volumes/Data/Users/dana";
/// A cache file `clean --category user-cache` proposes.
pub const CACHE_FILE: &str = "/System/Volumes/Data/Users/dana/Library/Caches/App/c.db";
/// Its contents, so a restore can be checked byte for byte.
pub const CACHE_CONTENTS: &[u8] = b"cached";
/// Where the default configuration puts the store on this machine.
pub const STORE: &str = "/System/Volumes/Data/Users/dana/.local/share/broza/quarantine";
/// The instant every command runs at.
pub const NOW: &str = "2026-09-21T10:36:08Z";

fn id(raw: &str) -> VolumeId {
    raw.parse().unwrap_or_else(|e| panic!("{e}"))
}

/// One internal disk with one APFS container holding the Data volume.
pub fn data_disk() -> Vec<Disk> {
    vec![Disk {
        id: id("disk0"),
        model: "SSD".into(),
        size_bytes: 1_000_000_000_000,
        internal: true,
        containers: vec![Container {
            id: id("disk3"),
            kind: FsKind::Apfs,
            size_bytes: 1_000_000_000_000,
            used_bytes: 500_000_000_000,
            free_bytes: 500_000_000_000,
            purgeable_bytes: 0,
            volumes: vec![Volume {
                id: id("disk3s5"),
                name: "Data".into(),
                uuid: None,
                role: VolumeRole::Data,
                mount_point: Some(PathBuf::from("/System/Volumes/Data")),
                used_bytes: 500_000_000_000,
                writable_by_broza: true,
                purpose: String::new(),
            }],
        }],
    }]
}

/// The fake machine: ports wired to fakes, plus the handles to steer them.
pub fn world() -> (Ports, Handles) {
    let (ports, handles) = broza::testing::fake_ports();
    handles.disks.set_disks(data_disk());
    handles.fs.add_root("/", 1);
    handles.fs.add_root("/System/Volumes/Data", 2);
    handles.fs.add_file(CACHE_FILE, CACHE_CONTENTS);
    handles.fs.set_size(CACHE_FILE, 900_000_000);
    handles
        .fs
        .add_file(format!("{HOME}/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw"), &[]);
    handles.clock.set(NOW.parse().unwrap_or_else(|e| panic!("{e}")));
    // A prompter with nothing queued: every answer a test wants is queued by
    // that test, and an unqueued prompt declines.
    let prompter = Arc::new(FakePrompter::scripted(&[]));
    let ports = Ports { prompter: Arc::clone(&prompter) as Arc<dyn Prompter>, ..ports };
    let handles = Handles { prompter, ..handles };
    (ports, handles)
}

/// The host block every envelope carries here.
pub fn host() -> Host {
    Host { macos_version: "26.1".into(), arch: "arm64".into() }
}

/// The instant every envelope is stamped with.
pub fn now() -> Timestamp {
    NOW.parse().unwrap_or_else(|e| panic!("{e}"))
}

/// Folder settings pointing at the fake home, cache off.
pub fn folders() -> FolderSettings {
    FolderSettings {
        home: Some(PathBuf::from(HOME)),
        cache_ttl: Duration::from_secs(60),
        no_cache: true,
        show_progress: false,
        verbose: false,
        own_stores: vec![PathBuf::from(STORE)],
    }
}

/// A store context over `ports` with the default configuration.
pub fn store_context<'a>(
    ports: &'a Ports,
    config: &'a Config,
    format: crate::output::OutputFormat,
) -> StoreContext<'a> {
    StoreContext {
        ports,
        config,
        host: host(),
        generated_at: now(),
        warnings: Vec::new(),
        policy: ColorPolicy::Never,
        format,
        home: Some(PathBuf::from(HOME)),
        uid_temp_dirs: Vec::new(),
    }
}
