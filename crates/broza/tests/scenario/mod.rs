//! The scenario the safety-kernel integration tests share.
//!
//! Built on the crate's own fakes (`broza::testing`, feature `test-support`) and on
//! [`mac_mount_table`], so the volumes, roles and devices are the ones a real Apple
//! Silicon Mac reports.
//!
//! Two test binaries include this module and each uses part of it.
#![allow(dead_code)]

use std::path::PathBuf;

use broza::clean::{PlanOutcome, Selection, plan_dry_run};
use broza::model::{Category, Finding, FindingPath, SessionId};
use broza::safety::guard::{Verdict, WriteRequest, approve};
use broza::safety::rejection::GuardRejection;
use broza::scan::MountTable;
use broza::testing::{FakeFileOps, mac_mount_table};
use broza::{BrozaError, ExitCode};

/// The home directory of the fake user.
pub const HOME: &str = "/Users/dana";
/// A cache file inside that home.
pub const CACHE: &str = "/Users/dana/Library/Caches/app.cache";
/// A second cache file, for size arithmetic.
pub const OTHER: &str = "/Users/dana/Library/Caches/other.cache";
/// The quarantine store of that home.
pub const STORE: &str = "/Users/dana/.local/share/broza/quarantine";
/// The same cache file spelled on the Data volume.
pub const TWIN: &str = "/System/Volumes/Data/Users/dana/Library/Caches/twin.cache";
/// Device of the Data volume in [`mac_mount_table`].
pub const DATA_DEVICE: u64 = 2;

/// The mount table of a stock Mac: System, Data (with firmlinks), VM, external.
pub fn mounts() -> MountTable {
    mac_mount_table()
}

/// A fixed session id, so failures are reproducible.
pub fn session() -> SessionId {
    "cln_20260921103608_a1b2".parse().unwrap_or_else(|error| panic!("{error}"))
}

/// A finding with the given paths and sizes.
pub fn finding(id: &str, category: Category, paths: &[(&str, u64)]) -> Finding {
    Finding::builder(id.parse().unwrap_or_else(|e| panic!("{e}")), category, "title")
        .paths(
            paths
                .iter()
                .map(|(path, size)| FindingPath {
                    path: PathBuf::from(path),
                    size_bytes: *size,
                    last_used: None,
                })
                .collect(),
        )
        .reclaimable_bytes(paths.iter().map(|(_, size)| *size).fold(0_u64, u64::saturating_add))
        .build()
        .unwrap_or_else(|error| panic!("{error}"))
}

/// One green `user-cache` finding covering the given paths.
pub fn caches(paths: &[(&str, u64)]) -> Vec<Finding> {
    vec![finding("user-cache.app", Category::UserCache, paths)]
}

/// The planner's outcome for a selection, or a panic with its error.
pub fn planned(findings: &[Finding], selection: &Selection) -> PlanOutcome {
    plan_dry_run(findings, selection, session(), None).unwrap_or_else(|error| panic!("{error}"))
}

/// A request that applies the plan on an interactive terminal.
pub fn applying() -> WriteRequest {
    WriteRequest { apply: true, tty: true, ..WriteRequest::new(HOME) }
}

/// The filesystem every safety-kernel test runs against.
///
/// The roots give each area the device of the volume that owns it in
/// [`mac_mount_table`], so `(device, inode)` in a token means what it says.
pub fn fs() -> FakeFileOps {
    FakeFileOps::new()
        .with_root("/", 1)
        .with_root("/Users", DATA_DEVICE)
        .with_root("/Library", DATA_DEVICE)
        .with_root("/System/Volumes/Data", DATA_DEVICE)
        .with_root("/System/Volumes/VM", 3)
        .with_root("/Volumes/External", 6)
        .with_sized_file(CACHE, 10)
        .with_sized_file(OTHER, 20)
        .with_sized_file("/System/Library/Caches/system.cache", 30)
        .with_sized_file("/System/Volumes/VM/swapfile0", 40)
        .with_sized_file(TWIN, 10)
        .with_sized_file("/Users/dana/Documents/report.pdf", 50)
        .with_sized_file("/Users/other/Documents/secret.txt", 1)
        .with_symlink("/Users/dana/Library/Caches/linked", CACHE)
        .with_symlink("/Users/dana/Library/evil", "/Users/dana/Library")
        .with_sized_file("/Volumes/External/.Trashes/501/old.dmg", 60)
        .with_sized_file("/System/Volumes/Data/private/var/db/.Trashes/victim", 70)
        .with_sized_file("/Users/dana/Library/Mobile Documents/synced.key", 70)
        .with_dir(STORE)
        .with_sized_file(format!("{STORE}/cln_20260921103608_a1b2/items/1/a"), 5)
}

/// Plans and approves the given cache paths.
pub fn approve_paths(paths: &[(&str, u64)], req: &WriteRequest) -> Result<Verdict, GuardRejection> {
    let findings = caches(paths);
    let plan = planned(&findings, &Selection::everything()).plan;
    approve(plan, &findings, req, &mounts(), &fs())
}

/// The rejection those paths produce, or a panic if they are approved.
pub fn rejection(paths: &[(&str, u64)], req: &WriteRequest) -> GuardRejection {
    match approve_paths(paths, req) {
        Err(rejection) => rejection,
        Ok(verdict) => panic!("expected a rejection, got {verdict:?}"),
    }
}

/// The exit code the CLI would return for a rejection.
pub fn exit_code(rejection: GuardRejection) -> ExitCode {
    ExitCode::from(&BrozaError::from(rejection))
}

/// The pending approval for one cache file, or a panic.
pub fn pending_or_panic(request: &WriteRequest) -> broza::safety::PendingApproval {
    match approve_paths(&[(CACHE, 10)], request) {
        Ok(Verdict::NeedsConfirmation(pending)) => pending,
        other => panic!("expected a pending approval, got {other:?}"),
    }
}
