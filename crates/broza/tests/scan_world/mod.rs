//! The fixture both scan pipeline suites share: a Data volume with a few known
//! files, a cache root, and the request that reports everything.
#![allow(dead_code)]

use std::path::PathBuf;
use std::time::Duration;

use broza::ports::Ports;
use broza::scan::{ScanRequest, VolumeScan, scan_volume};
use broza::testing::{FakeFileOps, Handles, fake_ports, mac_mount_table};

/// Mount point of the Data volume in the fixture.
pub const DATA: &str = "/System/Volumes/Data";
/// Device of the Data volume in the fixture.
pub const DATA_DEVICE: u64 = 2;
/// Mount point of the external volume in the fixture.
pub const EXTERNAL: &str = "/Volumes/External";
/// Device of the external volume in the fixture.
pub const EXTERNAL_DEVICE: u64 = 6;
/// Where the CLI would put the cache.
pub const CACHE_ROOT: &str = "/Users/dana/.cache/broza";
/// One hour, as a `cache-ttl`.
pub const AN_HOUR: Duration = Duration::from_secs(60 * 60);
/// Half of it.
pub const HALF_AN_HOUR: Duration = Duration::from_secs(30 * 60);
/// A minute past the hour.
pub const A_MINUTE: Duration = Duration::from_secs(60);
/// One megabyte, the size of each file of the pile.
pub const A_MEGABYTE: u64 = 1_000_000;
/// How many files the pile holds.
pub const FILES_IN_THE_PILE: usize = 10;
/// How many names the hard-link probe gives one file.
pub const LINKS_IN_THE_PROBE: usize = 40;
/// Store of the Data volume inside that cache root.
pub const DATA_STORE: &str = "/Users/dana/.cache/broza/v1/22222222-2222-4222-8222-222222222222/dirs.bin";

/// A request that reports everything, with the cache under [`CACHE_ROOT`].
pub fn request() -> ScanRequest {
    ScanRequest {
        min_size: 0,
        top: 10,
        depth: 2,
        cache_root: Some(PathBuf::from(CACHE_ROOT)),
        cache_ttl: Duration::from_secs(3600),
        ..ScanRequest::default()
    }
}

/// Ports whose filesystem holds a Data volume with a few known files.
pub fn ports() -> (Ports, Handles) {
    let (ports, handles) = fake_ports();
    let fs: &FakeFileOps = handles.fs.as_ref();
    fs.add_root(DATA, DATA_DEVICE);
    for (path, size) in [
        ("/System/Volumes/Data/Users/dana/Movies/film.mov", 5_000_000_u64),
        ("/System/Volumes/Data/Users/dana/Documents/notes.txt", 1_000_000),
        ("/System/Volumes/Data/Users/dana/Documents/deep/archive.zip", 2_000_000),
    ] {
        fs.add_file(path, &[]);
        fs.set_size(path, size);
    }
    (ports, handles)
}

pub fn scan_data(ports: &Ports, request: &ScanRequest) -> VolumeScan {
    scan_volume(&request.for_volume("disk3s5"), ports, &mac_mount_table(), None)
        .unwrap_or_else(|error| panic!("{error}"))
}

pub fn item_paths(scan: &VolumeScan) -> Vec<String> {
    scan.largest.iter().map(|item| item.path.display().to_string()).collect()
}
