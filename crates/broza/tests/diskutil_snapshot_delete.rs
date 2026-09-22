//! `DiskutilSnapshots::delete` against a scripted `tmutil`: the token gate, the
//! command it runs, and the two ways `tmutil` says no.
#![cfg(feature = "test-support")]

use std::path::Path;
use std::sync::Arc;

use broza::BrozaError;
use broza::adapters::DiskutilSnapshots;
use broza::clean::planner::{Selection, plan_dry_run};
use broza::model::{Category, Finding, Snapshot, VolumeId};
use broza::ports::{Answer, ProcessOutput, ProcessRunner, SnapshotProvider};
use broza::safety::guard::{
    Approved, SnapshotDelete, Verdict, Write, WriteRequest, approve, snapshot_deletions,
};
use broza::testing::{FakeFileOps, FakePrompter, FakeRunner, mac_mount_table};

const TMUTIL: &str = "/usr/bin/tmutil";
const NAME: &str = "com.apple.TimeMachine.2026-09-20-101530.local";
const DATE: &str = "2026-09-20-101530";

fn snapshot() -> Snapshot {
    Snapshot {
        name: NAME.to_owned(),
        uuid: None,
        purgeable: true,
        volume: Some("disk3s5".parse::<VolumeId>().unwrap_or_else(|e| panic!("{e}"))),
        mount_point: Some(Path::new("/System/Volumes/Data").to_path_buf()),
    }
}

/// A real token over one snapshot, through the planner and the guard.
fn token() -> Approved<SnapshotDelete> {
    let finding = Finding::builder(
        "snapshots.timemachine-local".parse().unwrap_or_else(|e| panic!("{e}")),
        Category::Snapshots,
        "Time Machine local snapshots",
    )
    .snapshots(vec![snapshot()])
    .item_count(1)
    .reasoning("size not reported by macOS")
    .build()
    .unwrap_or_else(|e| panic!("{e}"));
    let fs = FakeFileOps::new()
        .with_root("/", 1)
        .with_root("/Users", 2)
        .with_dir("/Users/dana/.local/share/broza");
    let session = "cln_20260921103608_a1b2".parse().unwrap_or_else(|e| panic!("{e}"));
    let root = Path::new("/Users/dana/.local/share/broza/quarantine");
    let outcome = plan_dry_run(std::slice::from_ref(&finding), &Selection::everything(), session, Some(root))
        .unwrap_or_else(|e| panic!("{e}"));
    let request = WriteRequest {
        apply: true,
        tty: true,
        quarantine_root: Some(root.to_path_buf()),
        ..WriteRequest::new("/Users/dana")
    };
    let approved: Approved<Write> = match approve(&outcome, &[finding], &request, &mac_mount_table(), &fs) {
        Ok(Verdict::NeedsConfirmation(pending)) => {
            pending.confirm(&FakePrompter::scripted(&[Answer::Yes])).unwrap_or_else(|e| panic!("{e}"))
        }
        other => panic!("expected a pending approval, got {other:?}"),
    };
    snapshot_deletions(&approved)
}

fn output(success: bool, stderr: &str) -> ProcessOutput {
    ProcessOutput {
        success,
        code: Some(i32::from(!success)),
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[test]
fn a_covered_snapshot_is_deleted_with_tmutil_by_its_date_stamp() {
    let runner =
        Arc::new(FakeRunner::new().with_output(TMUTIL, &["deletelocalsnapshots", DATE], output(true, "")));
    let provider = DiskutilSnapshots::new(Arc::clone(&runner) as Arc<dyn ProcessRunner>);
    let token = token();

    provider.delete(&token, &token.items()[0]).unwrap_or_else(|e| panic!("{e}"));

    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].program, TMUTIL);
    assert_eq!(calls[0].args, vec!["deletelocalsnapshots".to_owned(), DATE.to_owned()]);
}

#[test]
fn a_privilege_refusal_is_permission_denied_and_any_other_failure_keeps_tmutils_words() {
    let refused = Arc::new(FakeRunner::new().with_output(
        TMUTIL,
        &["deletelocalsnapshots", DATE],
        output(false, "tmutil: Operation not permitted"),
    ));
    let broken = Arc::new(FakeRunner::new().with_output(
        TMUTIL,
        &["deletelocalsnapshots", DATE],
        output(false, "Failed to delete local snapshot (error 22)"),
    ));
    let token = token();

    let denied = DiskutilSnapshots::new(refused as Arc<dyn ProcessRunner>).delete(&token, &token.items()[0]);
    let other = DiskutilSnapshots::new(broken as Arc<dyn ProcessRunner>).delete(&token, &token.items()[0]);

    assert!(matches!(denied, Err(BrozaError::PermissionDenied { .. })), "{denied:?}");
    assert!(matches!(&other, Err(BrozaError::Other(message)) if message.contains("error 22")), "{other:?}");
}

#[test]
fn an_item_the_token_does_not_cover_never_reaches_tmutil() {
    let runner = Arc::new(FakeRunner::new());
    let provider = DiskutilSnapshots::new(Arc::clone(&runner) as Arc<dyn ProcessRunner>);
    let token = token();
    // A second, independent approval over a different snapshot: not this token's item.
    let other = snapshot_deletions(&return_other_token());

    let refused = provider.delete(&token, &other.items()[0]);

    assert!(matches!(refused, Err(BrozaError::Other(_))), "{refused:?}");
    assert!(runner.calls().is_empty(), "nothing was run");
}

/// A token whose single item is a different snapshot on the same volume.
fn return_other_token() -> Approved<Write> {
    let finding = Finding::builder(
        "snapshots.timemachine-local".parse().unwrap_or_else(|e| panic!("{e}")),
        Category::Snapshots,
        "Time Machine local snapshots",
    )
    .snapshots(vec![Snapshot { name: "com.apple.TimeMachine.2026-09-01-000000.local".into(), ..snapshot() }])
    .item_count(1)
    .build()
    .unwrap_or_else(|e| panic!("{e}"));
    let fs = FakeFileOps::new()
        .with_root("/", 1)
        .with_root("/Users", 2)
        .with_dir("/Users/dana/.local/share/broza");
    let session = "cln_20260921103608_a1b2".parse().unwrap_or_else(|e| panic!("{e}"));
    let root = Path::new("/Users/dana/.local/share/broza/quarantine");
    let outcome = plan_dry_run(std::slice::from_ref(&finding), &Selection::everything(), session, Some(root))
        .unwrap_or_else(|e| panic!("{e}"));
    let request = WriteRequest {
        apply: true,
        tty: true,
        quarantine_root: Some(root.to_path_buf()),
        ..WriteRequest::new("/Users/dana")
    };
    match approve(&outcome, &[finding], &request, &mac_mount_table(), &fs) {
        Ok(Verdict::NeedsConfirmation(pending)) => {
            pending.confirm(&FakePrompter::scripted(&[Answer::Yes])).unwrap_or_else(|e| panic!("{e}"))
        }
        other => panic!("expected a pending approval, got {other:?}"),
    }
}
