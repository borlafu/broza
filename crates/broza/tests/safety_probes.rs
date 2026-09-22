//! Probes an adversarial review used against the safety kernel.
//!
//! Each test here is one way the kernel was, or could be, talked into approving
//! something it should refuse. They live together so the next reviewer can see
//! the whole attack surface at once. Only the `test-support` feature exposes
//! `broza::testing`: without it this file compiles to nothing.
#![cfg(feature = "test-support")]

mod scenario;

use std::path::{Path, PathBuf};

use broza::ExitCode;
use broza::clean::{PlanOutcome, Selection, plan_dry_run};
use broza::model::{Action, Category, CleanItem, CleanPlan, Finding, ItemStatus};
use broza::ports::Answer;
use broza::safety::Exclusions;
use broza::safety::guard::{Verdict, WritablePath, WriteRequest, approve, approve_quarantine_write};
use broza::safety::rejection::GuardRejection;
use broza::testing::{FakeFileOps, FakePrompter, mac_mount_table};

use scenario::{
    CACHE, HOME, STORE, applying, approve_paths, caches, exit_code, finding, fs, mounts, planned, session,
};

/// A finding whose reported path is a volume root would, as a prefix, vouch for
/// every path on the machine.
#[test]
fn a_finding_that_names_a_volume_root_vouches_for_nothing() {
    for root in ["/", "/System/Volumes/Data", "/Users"] {
        let finding = finding("user-cache.app", Category::UserCache, &[(root, 10)]);
        let stolen = forced_plan(&finding, "/Users/dana/Documents/report.pdf", 50, Action::Quarantine);
        let rejection = approve(&stolen, &[finding], &applying(), &mounts(), &fs())
            .err()
            .unwrap_or_else(|| panic!("`{root}` must not vouch for an unrelated path"));
        assert!(matches!(rejection, GuardRejection::Inconsistent(_)), "{root}: {rejection}");
        assert_eq!(exit_code(rejection), ExitCode::UsageError);
    }
}

/// The quarantine store is a second write target; it gets the same device
/// cross-check as everything else.
#[test]
fn a_quarantine_store_on_an_unexpected_device_is_refused() {
    let entry = PathBuf::from(format!("{STORE}/cln_20260921103608_a1b2/items/1/a"));
    let elsewhere = FakeFileOps::new()
        .with_root("/", 1)
        .with_root("/Users", 99)
        .with_dir(STORE)
        .with_sized_file(&entry, 5);
    let rejection = approve_quarantine_write(&[entry], Path::new(STORE), &mac_mount_table(), &elsewhere)
        .err()
        .unwrap_or_else(|| panic!("a store on the wrong device must be refused"));
    assert_eq!(rejection, GuardRejection::UnknownVolume(STORE.into()));
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

/// An entry that sits in the store but on another device is refused too.
#[test]
fn a_store_entry_on_an_unexpected_device_is_refused() {
    let entry = PathBuf::from(format!("{STORE}/cln_20260921103608_a1b2/items/1/a"));
    let mixed = FakeFileOps::new()
        .with_root("/", 1)
        .with_root("/Users", 2)
        .with_dir(STORE)
        .with_root(format!("{STORE}/cln_20260921103608_a1b2"), 99)
        .with_sized_file(&entry, 5);
    let rejection = approve_quarantine_write(&[entry], Path::new(STORE), &mac_mount_table(), &mixed)
        .err()
        .unwrap_or_else(|| panic!("an entry on the wrong device must be refused"));
    assert!(matches!(rejection, GuardRejection::UnknownVolume(_)), "{rejection}");
}

/// A quarantine root Broza may not write to would be an unchecked second target.
#[test]
fn an_impossible_quarantine_root_is_refused() {
    let refused = ["/", "/System/Volumes/Data", "relative/store", "/System/Library/store"];
    for root in refused {
        let findings = caches(&[(CACHE, 10)]);
        let outcome = planned(&findings, &Selection::everything());
        let request = WriteRequest { quarantine_root: Some(PathBuf::from(root)), ..applying() };
        let rejection = approve(&outcome, &findings, &request, &mounts(), &fs())
            .err()
            .unwrap_or_else(|| panic!("`{root}` must not be a quarantine store"));
        assert!(
            matches!(rejection, GuardRejection::InvalidRoot { .. } | GuardRejection::UnknownVolume(_)),
            "{root}: {rejection}"
        );
        assert_eq!(exit_code(rejection), ExitCode::UsageError);
    }
}

/// The store root gets the same device cross-check as a plan item.
#[test]
fn a_quarantine_root_on_an_unexpected_device_is_refused() {
    let store = "/Users/dana/.local/share/broza/store";
    let findings = caches(&[(CACHE, 10)]);
    let outcome = planned(&findings, &Selection::everything());
    let request = WriteRequest { quarantine_root: Some(PathBuf::from(store)), ..applying() };
    let filesystem = fs().with_root(store, 99).with_dir(store);
    let rejection = approve(&outcome, &findings, &request, &mounts(), &filesystem)
        .err()
        .unwrap_or_else(|| panic!("a store on an unexpected device must be refused"));
    assert_eq!(rejection, GuardRejection::UnknownVolume(store.into()));
}

/// An impossible home is reported before the store is even looked at.
#[test]
fn an_impossible_home_is_reported_even_when_a_store_is_configured() {
    let findings = caches(&[(CACHE, 10)]);
    let outcome = planned(&findings, &Selection::everything());
    let request = WriteRequest {
        apply: true,
        tty: true,
        quarantine_root: Some(PathBuf::from(STORE)),
        ..WriteRequest::new("/Users")
    };
    let rejection = approve(&outcome, &findings, &request, &mounts(), &fs())
        .err()
        .unwrap_or_else(|| panic!("`/Users` must not be a home"));
    assert!(matches!(rejection, GuardRejection::InvalidRoot { .. }), "{rejection}");
}

/// The store may live on an external disk, so that items from that disk can be
/// moved without crossing devices (`docs/cli-spec.md` §3.4).
#[test]
fn a_quarantine_store_on_an_external_disk_is_accepted() {
    let store = "/Volumes/External/.broza-quarantine";
    let findings = caches(&[(CACHE, 10)]);
    let outcome = planned(&findings, &Selection::everything());
    let request = WriteRequest { quarantine_root: Some(PathBuf::from(store)), ..applying() };
    let filesystem = fs().with_dir(store);
    let verdict = approve(&outcome, &findings, &request, &mounts(), &filesystem);
    assert!(matches!(verdict, Ok(Verdict::NeedsConfirmation(_))), "{verdict:?}");
}

/// The planner compares the quarantine root as a prefix, so a shape that could
/// never protect anything is refused rather than silently ignored.
#[test]
fn the_planner_refuses_a_quarantine_root_that_protects_nothing() {
    let findings = caches(&[(CACHE, 10)]);
    for root in ["relative/store", "/"] {
        let error = plan_dry_run(&findings, &Selection::everything(), session(), Some(Path::new(root)));
        assert!(error.is_err(), "{root} must be refused");
    }
}

/// A directory's size is the scanner's, not the kernel's; the token says so.
#[test]
fn a_directory_item_is_marked_as_not_measured() {
    let directory = "/Users/dana/Library/Caches/big";
    let findings = caches(&[(directory, 4096)]);
    let outcome = planned(&findings, &Selection::everything());
    let filesystem = fs().with_dir(directory);
    let pending = match approve(&outcome, &findings, &applying(), &mounts(), &filesystem) {
        Ok(Verdict::NeedsConfirmation(pending)) => pending,
        other => panic!("expected a pending approval, got {other:?}"),
    };
    let target = pending.items()[0].writable().unwrap_or_else(|| panic!("a path item"));
    assert!(!target.size_verified(), "a directory is never measured here");
    assert_eq!(pending.plan().items()[0].size_bytes, 4096, "the scanned size is kept");

    let file = match approve_paths(&[(CACHE, 10)], &applying()) {
        Ok(Verdict::NeedsConfirmation(pending)) => pending,
        other => panic!("expected a pending approval, got {other:?}"),
    };
    let measured = file.items()[0].writable().unwrap_or_else(|| panic!("a path item"));
    assert!(measured.size_verified(), "a file is measured by the guard");
}

/// Excluding a directory must protect what is inside it, whichever spelling the
/// user wrote and whichever spelling the plan carries.
#[test]
fn an_exclusion_on_a_directory_protects_its_contents() {
    let inside = "/Users/dana/Library/Caches/keep/deep/file";
    for pattern in [
        "/Users/dana/Library/Caches/keep",
        "/Users/dana/Library/Caches/keep/**",
        "/System/Volumes/Data/Users/dana/Library/Caches/keep",
    ] {
        let exclusions = Exclusions::new([pattern]).unwrap_or_else(|e| panic!("{e}"));
        let findings = caches(&[(inside, 10)]);
        let outcome = planned(&findings, &Selection::everything());
        let request = WriteRequest { exclusions, ..applying() };
        let filesystem = fs().with_sized_file(inside, 10);
        let rejection = approve(&outcome, &findings, &request, &mounts(), &filesystem)
            .err()
            .unwrap_or_else(|| panic!("`{pattern}` must protect `{inside}`"));
        assert_eq!(rejection, GuardRejection::Excluded(inside.into()), "{pattern}");
    }
}

/// The planner drops what an exclusion protects, subtree included.
#[test]
fn the_planner_drops_a_protected_subtree() {
    let exclusions =
        Exclusions::new(["/Users/dana/Library/Caches/keep/**"]).unwrap_or_else(|e| panic!("{e}"));
    let selection = Selection { exclusions, ..Selection::everything() };
    let findings = caches(&[
        ("/Users/dana/Library/Caches/keep", 10),
        ("/Users/dana/Library/Caches/keep/deep/file", 20),
        (CACHE, 30),
    ]);
    let outcome = plan_dry_run(&findings, &selection, session(), None).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(outcome.plan.items().len(), 1, "only the unprotected cache survives");
    assert_eq!(outcome.plan.planned_bytes(), 30);
}

/// A caller cannot have the report echo byte counters it invented.
#[test]
fn an_empty_verdict_carries_no_borrowed_numbers() {
    let inflated = CleanPlan::new(broza::model::CleanPlanRepr {
        dry_run: false,
        session_id: session(),
        planned_bytes: 0,
        quarantined_bytes: 999,
        reclaimed_bytes: 999,
        quarantine_path: Some(PathBuf::from(STORE)),
        expired_sessions: Vec::new(),
        items: Vec::new(),
    })
    .unwrap_or_else(|error| panic!("{error}"));
    let outcome = PlanOutcome { plan: inflated, informed_only: Vec::new(), informed_in_passing: Vec::new() };
    match approve(&outcome, &[], &applying(), &mounts(), &fs()) {
        Ok(Verdict::Nothing(plan)) => {
            assert_eq!(plan.quarantined_bytes(), 0);
            assert_eq!(plan.reclaimed_bytes(), 0);
            assert_eq!(plan.planned_bytes(), 0);
            assert_eq!(plan.quarantine_path(), None);
            assert!(!plan.is_dry_run(), "--apply was given");
        }
        other => panic!("expected nothing to do, got {other:?}"),
    }
}

/// The same, on the dry-run side of the fork.
#[test]
fn an_empty_dry_run_is_reported_as_a_dry_run() {
    let plan = CleanPlan::dry_run(session(), Vec::new()).unwrap_or_else(|error| panic!("{error}"));
    let outcome = PlanOutcome { plan, informed_only: Vec::new(), informed_in_passing: Vec::new() };
    match approve(&outcome, &[], &WriteRequest::new(HOME), &mounts(), &fs()) {
        Ok(Verdict::Nothing(plan)) => assert!(plan.is_dry_run()),
        other => panic!("expected nothing to do, got {other:?}"),
    }
}

/// Sanity: the ordinary path still works once all of the above is in place.
#[test]
fn an_ordinary_cache_cleanup_is_still_approved() {
    let approved = match approve_paths(&[(CACHE, 10)], &applying()) {
        Ok(Verdict::NeedsConfirmation(pending)) => {
            pending.confirm(&FakePrompter::always(Answer::Yes)).unwrap_or_else(|e| panic!("{e}"))
        }
        other => panic!("expected a pending approval, got {other:?}"),
    };
    assert_eq!(approved.items().len(), 1);
    assert_eq!(approved.items()[0].path(), Path::new(CACHE));
    assert!(approved.items()[0].writable().is_some_and(WritablePath::size_verified));
}

/// A plan built behind the planner's back, to probe one guard check directly.
fn forced_plan(finding: &Finding, path: &str, size_bytes: u64, action: Action) -> PlanOutcome {
    let plan = CleanPlan::dry_run(
        session(),
        vec![CleanItem {
            path: PathBuf::from(path),
            finding_id: finding.id().clone(),
            size_bytes,
            status: ItemStatus::Planned,
            action,
            error: None,
            snapshot: None,
        }],
    )
    .unwrap_or_else(|error| panic!("{error}"));
    PlanOutcome { plan, informed_only: Vec::new(), informed_in_passing: Vec::new() }
}
