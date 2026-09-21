//! Behavioural tests of the safety checks, from outside the crate.
//!
//! Checks 1 to 7 of `docs/cli-spec.md` §3.4: paths, volumes, roots, exclusions,
//! sizes and inform-only. The confirmation half lives in `safety_confirmation.rs`.
//! Only the public API is used here, exactly as `broza-cli` will use it, and only
//! the `test-support` feature exposes `broza::testing`: without it this file
//! compiles to nothing.
#![cfg(feature = "test-support")]

mod scenario;

use std::path::{Path, PathBuf};

use broza::ExitCode;
use broza::clean::{PlanOutcome, Selection};
use broza::model::{
    Action, Category, CleanItem, CleanPlan, Finding, FindingPath, Instructions, ItemErrorCode, ItemStatus,
    Risk, VolumeRole,
};
use broza::ports::Answer;
use broza::safety::guard::{Verdict, WriteRequest, approve};
use broza::safety::rejection::{GuardRejection, PolicyError};
use broza::safety::{Exclusions, RejectReason};
use broza::testing::FakePrompter;

use scenario::{
    CACHE, HOME, OTHER, STORE, applying, approve_paths, caches, exit_code, finding, fs, mounts,
    pending_or_panic, planned, rejection, session, unused_apps,
};

#[test]
fn a_firmlinked_home_path_resolves_to_the_data_volume_and_is_approved() {
    let verdict = approve_paths(&[(CACHE, 10)], &applying());
    assert!(matches!(verdict, Ok(Verdict::NeedsConfirmation(_))), "{verdict:?}");
    assert_eq!(mounts().role_for(Path::new(CACHE)), Some(VolumeRole::Data));
}

#[test]
fn the_data_volume_spelling_of_a_home_path_is_approved_too() {
    let twin = "/System/Volumes/Data/Users/dana/Library/Caches/twin.cache";
    assert_eq!(mounts().role_for(Path::new(twin)), Some(VolumeRole::Data));
    let verdict = approve_paths(&[(twin, 10)], &applying());
    assert!(matches!(verdict, Ok(Verdict::NeedsConfirmation(_))), "{verdict:?}");
}

#[test]
fn a_path_on_the_system_volume_is_rejected() {
    let rejection = rejection(&[("/System/Library/Caches/system.cache", 30)], &applying());
    assert_eq!(
        rejection,
        GuardRejection::ProtectedVolume {
            path: "/System/Library/Caches/system.cache".into(),
            role: VolumeRole::System,
        }
    );
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

#[test]
fn the_swap_file_on_the_vm_volume_is_rejected() {
    let rejection = rejection(&[("/System/Volumes/VM/swapfile0", 40)], &applying());
    assert_eq!(
        rejection,
        GuardRejection::ProtectedVolume { path: "/System/Volumes/VM/swapfile0".into(), role: VolumeRole::Vm }
    );
}

#[test]
fn a_symlinked_component_is_rejected() {
    let rejection = rejection(&[("/Users/dana/Library/Caches/linked", 10)], &applying());
    assert_eq!(rejection, GuardRejection::SymlinkComponent("/Users/dana/Library/Caches/linked".into()));
}

/// Regression: a `..` must never be collapsed, or the symlink it jumps over
/// would never be checked.
#[test]
fn a_path_that_walks_through_a_symlink_with_dot_dot_is_rejected() {
    let sneaky = "/Users/dana/Library/evil/../Caches/app.cache";
    let rejection = rejection(&[(sneaky, 10)], &applying());
    assert_eq!(rejection, GuardRejection::RelativeComponent(sneaky.into()));
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

#[test]
fn a_path_outside_the_allowlist_is_rejected() {
    let outside = "/Users/other/Documents/secret.txt";
    assert_eq!(mounts().role_for(Path::new(outside)), Some(VolumeRole::Data));
    let rejection = rejection(&[(outside, 1)], &applying());
    assert_eq!(rejection, GuardRejection::OutsideAllowedRoots(outside.into()));
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

#[test]
fn the_home_directory_itself_is_rejected_as_a_root() {
    let rejection = rejection(&[(HOME, 1)], &applying());
    assert_eq!(rejection, GuardRejection::RootItself(HOME.into()));
}

#[test]
fn a_home_that_is_not_a_home_directory_is_refused() {
    let findings = caches(&[(CACHE, 10)]);
    let outcome = planned(&findings, &Selection::everything());
    let request = WriteRequest { apply: true, tty: true, ..WriteRequest::new("/Users") };
    let rejection = approve(&outcome, &findings, &request, &mounts(), &fs())
        .err()
        .unwrap_or_else(|| panic!("`/Users` must not be an allowed root"));
    assert!(matches!(rejection, GuardRejection::InvalidRoot { .. }), "{rejection}");
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

#[test]
fn a_trash_at_the_root_of_a_non_system_volume_is_approved() {
    let findings =
        vec![finding("trash.volumes", Category::Trash, &[("/Volumes/External/.Trashes/501/old.dmg", 60)])];
    let outcome = planned(&findings, &Selection::everything());
    let verdict = approve(&outcome, &findings, &applying(), &mounts(), &fs());
    assert!(matches!(verdict, Ok(Verdict::NeedsConfirmation(_))), "{verdict:?}");
}

/// A `.Trashes` directory that is not at the root of a volume is just a folder.
#[test]
fn a_trashes_directory_buried_in_the_system_tree_is_rejected() {
    let victim = "/System/Volumes/Data/private/var/db/.Trashes/victim";
    let findings = vec![finding("trash.volumes", Category::Trash, &[(victim, 70)])];
    let outcome = planned(&findings, &Selection::everything());
    let rejection = approve(&outcome, &findings, &applying(), &mounts(), &fs())
        .err()
        .unwrap_or_else(|| panic!("a buried .Trashes is not a trash folder"));
    assert_eq!(rejection, GuardRejection::OutsideAllowedRoots(victim.into()));
}

#[test]
fn an_excluded_path_never_reaches_the_executor() {
    let exclusions = Exclusions::new(["/Users/dana/Library/Caches/**"]).unwrap_or_else(|e| panic!("{e}"));
    let rejection = rejection(&[(CACHE, 10)], &WriteRequest { exclusions, ..applying() });
    assert_eq!(rejection, GuardRejection::Excluded(CACHE.into()));
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

/// The two spellings of a path are the same directory, so one exclusion covers both.
#[test]
fn an_exclusion_written_once_protects_both_firmlink_spellings() {
    let exclusions = Exclusions::new(["/Users/dana/Library/Caches/**"]).unwrap_or_else(|e| panic!("{e}"));
    let twin = "/System/Volumes/Data/Users/dana/Library/Caches/twin.cache";
    let rejection = rejection(&[(twin, 10)], &WriteRequest { exclusions, ..applying() });
    assert_eq!(rejection, GuardRejection::Excluded(twin.into()));
}

#[test]
fn a_plan_larger_than_max_size_is_rejected() {
    let request = WriteRequest { max_size: Some(25), ..applying() };
    let rejection = rejection(&[(CACHE, 10), (OTHER, 20)], &request);
    assert_eq!(rejection, GuardRejection::MaxSizeExceeded { planned_bytes: 30, max_bytes: 25 });
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

#[test]
fn a_plan_exactly_at_max_size_is_approved() {
    let request = WriteRequest { max_size: Some(30), ..applying() };
    let verdict = approve_paths(&[(CACHE, 10), (OTHER, 20)], &request);
    assert!(matches!(verdict, Ok(Verdict::NeedsConfirmation(_))), "{verdict:?}");
}

/// A file that disappeared between the scan and the apply is skipped, not fatal.
#[test]
fn a_vanished_path_is_skipped_and_the_rest_of_the_plan_survives() {
    let request = WriteRequest { max_size: Some(10), ..applying() };
    let pending = match approve_paths(&[(CACHE, 10), ("/Users/dana/Library/Caches/gone", 999)], &request) {
        Ok(Verdict::NeedsConfirmation(pending)) => pending,
        other => panic!("expected a pending approval, got {other:?}"),
    };
    let items = pending.plan().items();
    assert_eq!(items[0].status, ItemStatus::Planned);
    assert_eq!(items[1].status, ItemStatus::Skipped);
    assert_eq!(items[1].error, Some(ItemErrorCode::NotFound));
    assert_eq!(pending.items().len(), 1, "only the surviving path is approved");
    assert_eq!(items[1].size_bytes, 0, "a skipped item frees nothing");
    assert_eq!(pending.plan().planned_bytes(), 10, "the total counts only what will be removed");
    assert_eq!(pending.request().total_bytes, 10, "a skipped item is not removed, so not counted");
}

#[test]
fn the_token_carries_the_checked_path_and_its_identity() {
    let approved = pending_or_panic(&applying())
        .confirm(&FakePrompter::always(Answer::Yes))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(approved.plan().items()[0].path, PathBuf::from(CACHE));
    assert_eq!(approved.items()[0].path(), Path::new(CACHE));
    assert_eq!(approved.items()[0].device(), 2, "the executor re-checks (device, inode)");
    assert!(approved.items()[0].inode() > 0);
    assert!(!approved.plan().is_dry_run(), "the approved plan is the one about to be applied");
}

#[test]
fn an_inform_only_selection_rejects_the_whole_plan_under_apply() {
    let cloud = Finding::builder(
        "cloud-synced.icloud".parse().unwrap_or_else(|e| panic!("{e}")),
        Category::CloudSynced,
        "iCloud Drive",
    )
    .paths(vec![FindingPath {
        path: "/Users/dana/Library/Mobile Documents/synced.key".into(),
        size_bytes: 70,
        last_used: None,
    }])
    .instructions(Instructions {
        provider: "iCloud Drive".into(),
        summary: "Use Apple's own feature.".into(),
        steps: Vec::new(),
    })
    .build()
    .unwrap_or_else(|error| panic!("{error}"));

    // The planner reports it instead of planning it, and a dry run stays exit 0.
    let outcome = planned(std::slice::from_ref(&cloud), &Selection::everything());
    assert!(outcome.plan.items().is_empty());
    assert_eq!(outcome.informed_only.len(), 1);

    // Under --apply, having asked for it at all refuses the whole plan.
    let rejection = approve(&outcome, std::slice::from_ref(&cloud), &applying(), &mounts(), &fs())
        .err()
        .unwrap_or_else(|| panic!("an inform-only selection must be refused"));
    assert_eq!(rejection, GuardRejection::InformOnlySelected(cloud.id().clone()));
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

/// `Docker.raw` is an inform-only sub-finding of a green category
/// (`docs/cli-spec.md` §3.3), so it reaches check 7 rather than the red refusal.
#[test]
fn an_inform_only_item_forced_into_a_plan_rejects_it_at_check_seven() {
    let docker = Finding::builder(
        "build-cache.docker".parse().unwrap_or_else(|e| panic!("{e}")),
        Category::BuildCache,
        "Docker.raw",
    )
    .action(Action::InformOnly)
    .instructions(Instructions {
        provider: "Docker Desktop".into(),
        summary: "Run `docker system prune`.".into(),
        steps: Vec::new(),
    })
    .build()
    .unwrap_or_else(|error| panic!("{error}"));

    let plan = CleanPlan::dry_run(
        session(),
        vec![CleanItem {
            path: CACHE.into(),
            finding_id: docker.id().clone(),
            size_bytes: 10,
            status: ItemStatus::Planned,
            action: Action::InformOnly,
            error: None,
        }],
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let forced = PlanOutcome { plan, informed_only: Vec::new(), informed_in_passing: Vec::new() };
    let rejection = approve(&forced, &[docker], &applying(), &mounts(), &fs())
        .err()
        .unwrap_or_else(|| panic!("an inform-only item must reject the plan"));
    assert_eq!(rejection, GuardRejection::InformOnlyItem(CACHE.into()));
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

/// The risk the guard enforces comes from the findings, never from the caller.
#[test]
fn a_red_finding_is_rejected_before_any_path_is_touched() {
    let red = Finding::builder(
        "unused-apps.leftovers".parse().unwrap_or_else(|e| panic!("{e}")),
        Category::UnusedApps,
        "leftovers",
    )
    .risk(Risk::Red)
    .paths(vec![FindingPath { path: "/etc/nowhere".into(), size_bytes: 1, last_used: None }])
    .reclaimable_bytes(1)
    .build()
    .unwrap_or_else(|error| panic!("{error}"));
    let findings = vec![red];
    let outcome = planned(&findings, &Selection::everything());
    let rejection = approve(&outcome, &findings, &applying(), &mounts(), &fs())
        .err()
        .unwrap_or_else(|| panic!("a red selection must be refused"));
    assert_eq!(
        rejection,
        GuardRejection::Policy(PolicyError::Rejected(RejectReason::RedNotActionable)),
        "the red refusal wins over the unreachable path"
    );
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

/// A plan built without `--purge` must not be executed with it, or the user
/// would quarantine what they asked to destroy and the other way round.
#[test]
fn a_plan_and_the_purge_flag_must_agree() {
    let findings = caches(&[(CACHE, 10)]);
    let outcome = planned(&findings, &Selection::everything());
    let request = WriteRequest { purge: true, ..applying() };
    let rejection = approve(&outcome, &findings, &request, &mounts(), &fs())
        .err()
        .unwrap_or_else(|| panic!("a quarantine item under --purge is inconsistent"));
    assert!(matches!(rejection, GuardRejection::Inconsistent(_)), "{rejection}");

    let purging = Selection { purge: true, ..Selection::everything() };
    let purged = planned(&findings, &purging);
    let rejection = approve(&purged, &findings, &applying(), &mounts(), &fs())
        .err()
        .unwrap_or_else(|| panic!("a purge item without --purge is inconsistent"));
    assert!(matches!(rejection, GuardRejection::Inconsistent(_)), "{rejection}");
}

/// An item may only name a path its own finding reported, or something inside
/// one. Otherwise any item could borrow a permissive finding's category.
#[test]
fn an_item_cannot_borrow_a_path_the_finding_never_reported() {
    let findings = unused_apps(&[("/Applications/Other.app", 10)]);
    let stolen = forced_plan(&findings[0], "/Applications/Safari.app", 10, Action::Quarantine);
    let rejection = approve(&stolen, &findings, &applying(), &mounts(), &fs())
        .err()
        .unwrap_or_else(|| panic!("Safari was never part of that finding"));
    assert!(matches!(rejection, GuardRejection::Inconsistent(_)), "{rejection}");
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

/// The same trick with a trash finding, which would otherwise reach a document.
#[test]
fn a_document_cannot_ride_along_with_a_trash_finding() {
    let findings =
        vec![finding("trash.volumes", Category::Trash, &[("/Volumes/External/.Trashes/501/old.dmg", 60)])];
    let stolen = forced_plan(&findings[0], "/Users/dana/Documents/report.pdf", 50, Action::Purge);
    let rejection = approve(&stolen, &findings, &applying(), &mounts(), &fs())
        .err()
        .unwrap_or_else(|| panic!("a document is not in the trash"));
    assert!(matches!(rejection, GuardRejection::Inconsistent(_)), "{rejection}");
}

/// A plan that under-reports a file would slip past `--max-size`.
#[test]
fn an_item_that_claims_less_than_the_file_holds_is_refused() {
    let request = WriteRequest { max_size: Some(5), ..applying() };
    let rejection = rejection(&[(CACHE, 1)], &request);
    assert!(matches!(rejection, GuardRejection::Inconsistent(_)), "{rejection}");
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

/// The size the cap and the summary use is the one `lstat` reported.
#[test]
fn the_observed_size_is_what_counts() {
    let generous = WriteRequest { max_size: Some(100), ..applying() };
    let pending = match approve_paths(&[(CACHE, 40)], &generous) {
        Ok(Verdict::NeedsConfirmation(pending)) => pending,
        other => panic!("expected a pending approval, got {other:?}"),
    };
    assert_eq!(pending.plan().planned_bytes(), 10, "the file holds 10 bytes, not the claimed 40");
    assert_eq!(pending.request().total_bytes, 10);
}

/// The quarantine store holds what the last run moved aside; a plan that reached
/// into it would delete the user's safety net.
#[test]
fn an_item_inside_the_quarantine_store_is_refused() {
    let entry = format!("{STORE}/cln_20260921103608_a1b2/items/1/a");
    let findings = caches(&[(entry.as_str(), 5)]);
    let outcome = planned(&findings, &Selection::everything());
    let request = WriteRequest { quarantine_root: Some(STORE.into()), ..applying() };
    let rejection = approve(&outcome, &findings, &request, &mounts(), &fs())
        .err()
        .unwrap_or_else(|| panic!("the store is never cleaned by a plan"));
    assert!(matches!(rejection, GuardRejection::InsideQuarantineStore { .. }), "{rejection}");
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

/// A plan built behind the planner's back, to probe a guard check directly.
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
        }],
    )
    .unwrap_or_else(|error| panic!("{error}"));
    PlanOutcome { plan, informed_only: Vec::new(), informed_in_passing: Vec::new() }
}
