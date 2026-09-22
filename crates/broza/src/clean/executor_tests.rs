//! The executor against the real guard and the fake filesystem: purge items
//! vanish for good, quarantine items still land in a session, and a mixed plan
//! does both without confusing the two.

use std::path::Path;

use crate::clean::executor::execute;
use crate::clean::planner::{Selection, plan_dry_run};
use crate::model::{Action, Category, Finding, FindingPath, ItemErrorCode, ItemStatus};
use crate::ports::{Answer, FileOps};
use crate::quarantine::MoveRequest;
use crate::quarantine::fixtures::{HOME, NOW, ROOT, TTL, at, session_id, store_fs};
use crate::safety::guard::{Approved, Verdict, Write, WriteRequest, approve};
use crate::testing::{FakeFileOps, FakePrompter, FixedClock, mac_mount_table};

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
    let selection = Selection { purge, ..Selection::everything() };
    let outcome = plan_dry_run(findings, &selection, session_id(), Some(Path::new(ROOT)))
        .unwrap_or_else(|e| panic!("{e}"));
    let request = WriteRequest {
        apply: true,
        tty: true,
        purge,
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

    let executed = execute(&token, &MoveRequest { ttl: TTL, max_size: None }, &fs, &FixedClock::at(at(NOW)))
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

    let executed = execute(&token, &MoveRequest { ttl: TTL, max_size: None }, &fs, &FixedClock::at(at(NOW)))
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

    let executed = execute(&token, &MoveRequest { ttl: TTL, max_size: None }, &fs, &FixedClock::at(at(NOW)))
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

    let executed = execute(&token, &MoveRequest { ttl: TTL, max_size: None }, &fs, &FixedClock::at(at(NOW)))
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status_of(&executed.plan, TRASH).0, ItemStatus::Failed);
    assert_eq!(status_of(&executed.plan, TRASH).1.map(|c| c.to_string()), Some("changed_since_check".into()));
    assert!(fs.exists(Path::new(TRASH)), "the impostor stays");
    assert_eq!(executed.plan.reclaimed_bytes(), 0);
}
