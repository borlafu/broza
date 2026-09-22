//! The executor against the real guard and the fake filesystem: purge items
//! vanish for good, quarantine items still land in a session, and a mixed plan
//! does both without confusing the two.

use std::path::Path;

use crate::clean::executor::execute;
use crate::clean::planner::{Selection, plan_dry_run};
use crate::model::{Action, Category, Finding, FindingPath, ItemErrorCode, ItemStatus};
use crate::model::{Snapshot, VolumeId};
use crate::ports::{Answer, FileOps};
use crate::quarantine::MoveRequest;
use crate::quarantine::fixtures::{HOME, NOW, ROOT, TTL, at, session_id, store_fs};
use crate::safety::guard::{Approved, Verdict, Write, WriteRequest, approve};
use crate::testing::{FakeFileOps, FakePrompter, FakeSnapshots, FixedClock, mac_mount_table};

const TRASH: &str = "/Users/dana/.Trash/old-movie.mp4";
const TRASH_DIR: &str = "/Users/dana/.Trash/Project";
const CACHE: &str = "/Users/dana/Library/Caches/app.cache";

fn fs() -> FakeFileOps {
    let fs = store_fs();
    for (path, size) in
        [(TRASH, 4000_u64), (format!("{TRASH_DIR}/src/main.rs").as_str(), 2000), (CACHE, 3000)]
    {
        fs.add_file(path, b"x");
        fs.set_size(path, size);
    }
    fs
}

fn finding(id: &str, category: Category, paths: &[&str], fs: &FakeFileOps) -> Finding {
    let reported: Vec<FindingPath> = paths
        .iter()
        .map(|path| FindingPath {
            path: Path::new(path).to_path_buf(),
            size_bytes: fs.metadata(Path::new(path)).map_or(0, |m| m.allocated_bytes),
            last_used: None,
        })
        .collect();
    let bytes = reported.iter().map(|p| p.size_bytes).sum();
    Finding::builder(id.parse().unwrap_or_else(|e| panic!("{e}")), category, id)
        .paths(reported)
        .reclaimable_bytes(bytes)
        .build()
        .unwrap_or_else(|e| panic!("{e}"))
}

fn approved(fs: &FakeFileOps, findings: &[Finding], purge: bool) -> Approved<Write> {
    approved_with(fs, findings, purge, None)
}

fn approved_with(
    fs: &FakeFileOps,
    findings: &[Finding],
    purge: bool,
    max_size: Option<u64>,
) -> Approved<Write> {
    let selection = Selection { purge, ..Selection::everything() };
    let outcome = plan_dry_run(findings, &selection, session_id(), Some(Path::new(ROOT)))
        .unwrap_or_else(|e| panic!("{e}"));
    let request = WriteRequest {
        apply: true,
        tty: true,
        purge,
        max_size,
        quarantine_root: Some(Path::new(ROOT).to_path_buf()),
        ..WriteRequest::new(HOME)
    };
    match approve(&outcome, findings, &request, &mac_mount_table(), fs) {
        Ok(Verdict::NeedsConfirmation(pending)) => {
            pending.confirm(&FakePrompter::scripted(&[Answer::Yes])).unwrap_or_else(|e| panic!("{e}"))
        }
        other => panic!("expected a pending approval, got {other:?}"),
    }
}

fn status_of(plan: &crate::model::CleanPlan, path: &str) -> (ItemStatus, Option<ItemErrorCode>) {
    let item =
        plan.items().iter().find(|item| item.path == Path::new(path)).unwrap_or_else(|| panic!("{path}"));
    (item.status.clone(), item.error.clone())
}

#[test]
fn a_trash_plan_purges_its_items_frees_their_bytes_and_creates_no_session() {
    let fs = fs();
    let findings = vec![finding("trash.home", Category::Trash, &[TRASH, TRASH_DIR], &fs)];
    let token = approved(&fs, &findings, false);

    let executed = execute(
        &token,
        &MoveRequest { ttl: TTL, max_size: None },
        &fs,
        &FixedClock::at(at(NOW)),
        &FakeSnapshots::new(),
    )
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status_of(&executed.plan, TRASH), (ItemStatus::Purged, None));
    assert_eq!(status_of(&executed.plan, TRASH_DIR), (ItemStatus::Purged, None));
    assert!(executed.session.is_none(), "nothing was quarantined");
    assert!(executed.plan.quarantine_path().is_none());
    assert_eq!(executed.plan.quarantined_bytes(), 0);
    assert_eq!(executed.plan.reclaimed_bytes(), 4096 + 4096, "the file's block and the tree's, measured");
    assert!(!fs.exists(Path::new(TRASH)) && !fs.exists(Path::new(TRASH_DIR)));
    assert!(executed.plan.items().iter().all(|item| item.action == Action::Purge));
}

#[test]
fn a_mixed_plan_quarantines_the_cache_and_purges_the_trash() {
    let fs = fs();
    let findings = vec![
        finding("user-cache.app", Category::UserCache, &[CACHE], &fs),
        finding("trash.home", Category::Trash, &[TRASH], &fs),
    ];
    let token = approved(&fs, &findings, false);

    let executed = execute(
        &token,
        &MoveRequest { ttl: TTL, max_size: None },
        &fs,
        &FixedClock::at(at(NOW)),
        &FakeSnapshots::new(),
    )
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status_of(&executed.plan, CACHE).0, ItemStatus::Quarantined);
    assert_eq!(status_of(&executed.plan, TRASH).0, ItemStatus::Purged);
    let session = executed.session.unwrap_or_else(|| panic!("a session for the cache"));
    assert_eq!(session.entries.len(), 1, "only the quarantine item is in the manifest");
    assert_eq!(session.entries[0].original_path, Path::new(CACHE));
    // The mover reports the guard-verified apparent size of a file (3000 here);
    // the purger reports allocated blocks. Both are what the code measured.
    assert_eq!(executed.plan.quarantined_bytes(), 3000);
    assert_eq!(executed.plan.reclaimed_bytes(), 4096);
    assert!(executed.plan.quarantine_path().is_some());
    assert!(!fs.exists(Path::new(TRASH)) && !fs.exists(Path::new(CACHE)));
}

#[test]
fn purge_upgrades_a_quarantine_finding_when_the_flag_is_given() {
    let fs = fs();
    let findings = vec![finding("user-cache.app", Category::UserCache, &[CACHE], &fs)];
    let token = approved(&fs, &findings, true);

    let executed = execute(
        &token,
        &MoveRequest { ttl: TTL, max_size: None },
        &fs,
        &FixedClock::at(at(NOW)),
        &FakeSnapshots::new(),
    )
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status_of(&executed.plan, CACHE), (ItemStatus::Purged, None));
    assert!(executed.session.is_none());
    assert!(!fs.exists(Path::new(CACHE)));
}

#[test]
fn an_item_replaced_since_the_check_is_not_purged() {
    let fs = fs();
    let findings = vec![finding("trash.home", Category::Trash, &[TRASH], &fs)];
    let token = approved(&fs, &findings, false);
    fs.remove_tree(Path::new(TRASH)).unwrap_or_else(|e| panic!("{e}"));
    fs.add_file(TRASH, b"an impostor");

    let executed = execute(
        &token,
        &MoveRequest { ttl: TTL, max_size: None },
        &fs,
        &FixedClock::at(at(NOW)),
        &FakeSnapshots::new(),
    )
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status_of(&executed.plan, TRASH).0, ItemStatus::Failed);
    assert_eq!(status_of(&executed.plan, TRASH).1.map(|c| c.to_string()), Some("changed_since_check".into()));
    assert!(fs.exists(Path::new(TRASH)), "the impostor stays");
    assert_eq!(executed.plan.reclaimed_bytes(), 0);
}

fn data_snapshot(name: &str, purgeable: bool) -> Snapshot {
    Snapshot {
        name: name.to_owned(),
        uuid: Some(format!("00000021-1111-4222-8333-{:012}", name.len() as u64 * 7919 % 1_000_000_007)),
        purgeable,
        volume: Some("disk3s5".parse::<VolumeId>().unwrap_or_else(|e| panic!("{e}"))),
        mount_point: Some(Path::new("/System/Volumes/Data").to_path_buf()),
    }
}

fn snapshots_finding(snapshots: Vec<Snapshot>) -> Finding {
    let count = u64::try_from(snapshots.len()).unwrap_or(u64::MAX);
    Finding::builder(
        "snapshots.timemachine-local".parse().unwrap_or_else(|e| panic!("{e}")),
        Category::Snapshots,
        "Time Machine local snapshots",
    )
    .snapshots(snapshots)
    .item_count(count)
    .reasoning("size not reported by macOS")
    .build()
    .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn purgeable_time_machine_snapshots_are_deleted_by_name_and_only_those() {
    let fs = fs();
    let listed = vec![
        data_snapshot("com.apple.TimeMachine.2026-09-20-101530.local", true),
        data_snapshot("com.apple.TimeMachine.2026-09-19-101530.local", false),
        data_snapshot("com.apple.os.update-abc", false),
    ];
    let provider = FakeSnapshots::new()
        .with_snapshots("disk3s5".parse().unwrap_or_else(|e| panic!("{e}")), listed.clone());
    let findings = vec![snapshots_finding(listed)];
    let token = approved(&fs, &findings, false);

    let executed =
        execute(&token, &MoveRequest { ttl: TTL, max_size: None }, &fs, &FixedClock::at(at(NOW)), &provider)
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(provider.deleted(), vec!["com.apple.TimeMachine.2026-09-20-101530.local".to_owned()]);
    assert_eq!(executed.plan.items().len(), 1, "only the purgeable Time Machine snapshot was planned");
    assert_eq!(executed.plan.items()[0].status, ItemStatus::Purged);
    assert_eq!(executed.plan.items()[0].action, Action::TmutilDelete);
    assert_eq!(
        executed.plan.items()[0].snapshot.as_ref().map(|s| s.name.as_str()),
        Some("com.apple.TimeMachine.2026-09-20-101530.local")
    );
    assert!(executed.session.is_none());
    assert_eq!(executed.plan.reclaimed_bytes(), 0, "macOS reports no snapshot size");
}

#[test]
fn a_snapshot_deletion_tmutil_refuses_fails_the_item_and_names_the_command() {
    let fs = fs();
    let listed = vec![data_snapshot("com.apple.TimeMachine.2026-09-20-101530.local", true)];
    let provider = FakeSnapshots::new()
        .with_snapshots("disk3s5".parse().unwrap_or_else(|e| panic!("{e}")), listed.clone());
    provider.refuse_deletions();
    let token = approved(&fs, &[snapshots_finding(listed)], false);

    let executed =
        execute(&token, &MoveRequest { ttl: TTL, max_size: None }, &fs, &FixedClock::at(at(NOW)), &provider)
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(executed.plan.items()[0].status, ItemStatus::Failed);
    assert_eq!(executed.plan.items()[0].error, Some(ItemErrorCode::PermissionDenied));
    assert_eq!(executed.warnings.len(), 1);
    assert!(
        executed.warnings[0]
            .message
            .contains("sudo diskutil apfs deleteSnapshot disk3s5 -uuid 00000021-1111-4222-8333-"),
        "{:?}",
        executed.warnings
    );
    assert!(provider.deleted().is_empty());
}

const CACHE2: &str = "/Users/dana/Library/Caches/other.cache";

fn run(token: &Approved<Write>, fs: &FakeFileOps, max_size: Option<u64>) -> crate::clean::Executed {
    execute(token, &MoveRequest { ttl: TTL, max_size }, fs, &FixedClock::at(at(NOW)), &FakeSnapshots::new())
        .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn a_failed_quarantine_item_before_a_purge_does_not_shift_the_purge_onto_another_item() {
    let fs = fs();
    fs.add_file(CACHE2, b"y");
    fs.set_size(CACHE2, 3000);
    let findings = vec![
        finding("user-cache.app", Category::UserCache, &[CACHE, CACHE2], &fs),
        finding("trash.home", Category::Trash, &[TRASH], &fs),
    ];
    let token = approved(&fs, &findings, false);
    // The first cache is replaced after the check: the mover fails it and the
    // plan's first slot ends `failed`; the purge must still land on the trash.
    fs.remove_tree(Path::new(CACHE)).unwrap_or_else(|e| panic!("{e}"));
    fs.add_file(CACHE, b"an impostor");

    let executed = run(&token, &fs, None);

    assert_eq!(status_of(&executed.plan, CACHE).0, ItemStatus::Failed);
    assert_eq!(status_of(&executed.plan, CACHE2).0, ItemStatus::Quarantined);
    assert_eq!(status_of(&executed.plan, TRASH), (ItemStatus::Purged, None));
    assert!(fs.exists(Path::new(CACHE)), "the impostor stays");
    assert!(!fs.exists(Path::new(TRASH)));
}

#[test]
fn a_trash_item_on_a_protected_volume_is_refused_by_the_guard_before_anything_runs() {
    let vm_device = mac_mount_table()
        .volume_for(Path::new("/System/Volumes/VM"))
        .map_or_else(|| panic!("the fake table knows the VM volume"), |entry| entry.device);
    let fs = FakeFileOps::new()
        .with_root("/", 1)
        .with_root("/Users", 2)
        .with_root("/System/Volumes/VM", vm_device);
    let vm_trash = "/System/Volumes/VM/.Trashes/501/swap.bin";
    fs.add_file(vm_trash, b"x");
    fs.add_dir(ROOT);
    let findings = vec![finding("trash.external-volumes", Category::Trash, &[vm_trash], &fs)];
    let outcome = plan_dry_run(&findings, &Selection::everything(), session_id(), Some(Path::new(ROOT)))
        .unwrap_or_else(|e| panic!("{e}"));
    let request = WriteRequest {
        apply: true,
        tty: true,
        quarantine_root: Some(Path::new(ROOT).to_path_buf()),
        ..WriteRequest::new(HOME)
    };

    let verdict = approve(&outcome, &findings, &request, &mac_mount_table(), &fs);

    assert!(matches!(verdict, Err(crate::safety::GuardRejection::ProtectedVolume { .. })), "{verdict:?}");
    assert!(fs.exists(Path::new(vm_trash)));
}

#[test]
fn a_removal_the_filesystem_refuses_fails_the_item_and_the_run_goes_on() {
    let fs = fs();
    let findings = vec![finding("trash.home", Category::Trash, &[TRASH, TRASH_DIR], &fs)];
    let token = approved(&fs, &findings, false);
    fs.add_denied(TRASH_DIR);

    let executed = run(&token, &fs, None);

    assert_eq!(status_of(&executed.plan, TRASH), (ItemStatus::Purged, None));
    assert_eq!(status_of(&executed.plan, TRASH_DIR).0, ItemStatus::Failed);
    assert_eq!(status_of(&executed.plan, TRASH_DIR).1, Some(ItemErrorCode::PermissionDenied));
    assert_eq!(executed.plan.reclaimed_bytes(), 4096, "only the file that went");
}

#[test]
fn the_size_cap_applies_to_purges_and_counts_what_was_quarantined_first() {
    let fs = fs();
    let findings = vec![
        finding("user-cache.app", Category::UserCache, &[CACHE], &fs),
        finding("trash.home", Category::Trash, &[TRASH, TRASH_DIR], &fs),
    ];
    // Planned 3000 + 4096 + 4096 stays under the 12 000-byte cap for the pre-check;
    // moved 3000 + purged 4096 = 7096, and the tree's 4096 more would pass it.
    let token = approved_with(&fs, &findings, false, Some(11_000));

    let executed = run(&token, &fs, Some(11_000));

    assert_eq!(status_of(&executed.plan, CACHE).0, ItemStatus::Quarantined);
    assert_eq!(status_of(&executed.plan, TRASH), (ItemStatus::Purged, None));
    assert_eq!(status_of(&executed.plan, TRASH_DIR).0, ItemStatus::Skipped);
    assert_eq!(
        status_of(&executed.plan, TRASH_DIR).1.map(|c| c.to_string()),
        Some("max_size_exceeded".into())
    );
    assert!(fs.exists(Path::new(TRASH_DIR)), "left in place");
}

#[test]
fn a_hard_linked_file_frees_nothing_when_purged() {
    let fs = fs();
    fs.add_hard_link(TRASH, "/Users/dana/Documents/still-here.mp4");
    let findings = vec![finding("trash.home", Category::Trash, &[TRASH], &fs)];
    let token = approved(&fs, &findings, false);

    let executed = run(&token, &fs, None);

    assert_eq!(status_of(&executed.plan, TRASH), (ItemStatus::Purged, None));
    assert_eq!(executed.plan.reclaimed_bytes(), 0, "the other name keeps the blocks");
}
