//! Behavioural tests of the safety kernel, from outside the crate.
//!
//! Only the public API is used here, exactly as `broza-cli` will use it. The fakes
//! are local to this file so the test does not depend on the shared `testing`
//! module (`docs/implementation-plan.md` §3.6).

mod fakes;

use std::path::{Path, PathBuf};

use broza::clean::{Selection, max_risk, plan_dry_run};
use broza::model::{
    Action, Category, CleanPlan, Finding, FindingPath, Instructions, Risk, SessionId, VolumeRole,
};
use broza::ports::Answer;
use broza::safety::guard::{Verdict, WriteRequest, approve, approve_quarantine_write};
use broza::safety::rejection::{GuardRejection, PolicyError};
use broza::safety::{Exclusions, RejectReason};
use broza::{BrozaError, ExitCode};

use fakes::{FakePrompter, MemFs, mount_table};

const HOME: &str = "/Users/dana";
const CACHE: &str = "/Users/dana/Library/Caches/app.cache";
const STORE: &str = "/Users/dana/.local/share/broza/quarantine";

fn session() -> SessionId {
    "cln_20260921103608_a1b2".parse().unwrap_or_else(|error| panic!("{error}"))
}

fn finding(id: &str, category: Category, paths: &[(&str, u64)]) -> Finding {
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
        .reclaimable_bytes(paths.iter().map(|(_, size)| size).sum())
        .build()
        .unwrap_or_else(|error| panic!("{error}"))
}

fn plan_for(paths: &[(&str, u64)]) -> CleanPlan {
    let findings = [finding("user-cache.app", Category::UserCache, paths)];
    plan_dry_run(&findings, &Selection::everything(), session(), None)
        .unwrap_or_else(|error| panic!("{error}"))
}

/// A request that applies the plan on an interactive terminal.
fn applying() -> WriteRequest {
    WriteRequest { apply: true, tty: true, max_risk: Some(Risk::Green), ..WriteRequest::new(HOME) }
}

fn fs() -> MemFs {
    MemFs::new()
        .file(CACHE, 10)
        .file("/Users/dana/Library/Caches/other.cache", 20)
        .file("/System/Library/Caches/system.cache", 30)
        .file("/System/Volumes/VM/swapfile0", 40)
        .file("/System/Volumes/Data/Users/dana/Library/Caches/twin.cache", 10)
        .file("/Users/dana/Documents/report.pdf", 50)
        .file("/Users/other/Documents/secret.txt", 1)
        .symlink("/Users/dana/Library/Caches/linked")
        .file("/Volumes/External/.Trashes/501/old.dmg", 60)
        .file("/Users/dana/Library/Mobile Documents/synced.key", 70)
        .dir(STORE)
        .file("/Users/dana/.local/share/broza/quarantine/cln_20260921103608_a1b2/items/1/a", 5)
}

fn approve_paths(paths: &[(&str, u64)], req: &WriteRequest) -> Result<Verdict, GuardRejection> {
    approve(plan_for(paths), req, &mount_table(), &fs())
}

fn rejection(paths: &[(&str, u64)], req: &WriteRequest) -> GuardRejection {
    match approve_paths(paths, req) {
        Err(rejection) => rejection,
        Ok(verdict) => panic!("expected a rejection, got {verdict:?}"),
    }
}

fn exit_code(rejection: GuardRejection) -> ExitCode {
    ExitCode::from(&BrozaError::from(rejection))
}

#[test]
fn without_apply_the_verdict_is_always_a_dry_run() {
    let verdict = approve_paths(&[(CACHE, 10)], &WriteRequest::new(HOME));
    match verdict {
        Ok(Verdict::DryRun(plan)) => {
            assert!(plan.is_dry_run());
            assert_eq!(plan.planned_bytes(), 10);
        }
        other => panic!("expected a dry run, got {other:?}"),
    }
}

#[test]
fn a_firmlinked_home_path_resolves_to_the_data_volume_and_is_approved() {
    let verdict = approve_paths(&[(CACHE, 10)], &applying());
    assert!(matches!(verdict, Ok(Verdict::NeedsConfirmation(_))), "{verdict:?}");
    assert_eq!(mount_table().role_for(Path::new(CACHE)), Some(VolumeRole::Data));
}

#[test]
fn the_data_volume_spelling_of_a_home_path_is_approved_too() {
    let twin = "/System/Volumes/Data/Users/dana/Library/Caches/twin.cache";
    assert_eq!(mount_table().role_for(Path::new(twin)), Some(VolumeRole::Data));
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
    assert_eq!(exit_code(rejection), ExitCode::PermissionDenied);
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

#[test]
fn a_path_outside_the_allowlist_is_rejected() {
    let outside = "/Users/other/Documents/secret.txt";
    assert_eq!(mount_table().role_for(Path::new(outside)), Some(VolumeRole::Data));
    let rejection = rejection(&[(outside, 1)], &applying());
    assert_eq!(rejection, GuardRejection::OutsideAllowedRoots(outside.into()));
    assert_eq!(exit_code(rejection), ExitCode::PermissionDenied);
}

#[test]
fn the_home_directory_itself_is_rejected_as_a_root() {
    let rejection = rejection(&[(HOME, 1)], &applying());
    assert_eq!(rejection, GuardRejection::RootItself(HOME.into()));
}

#[test]
fn a_trash_on_a_non_system_volume_is_approved() {
    let findings =
        [finding("trash.volumes", Category::Trash, &[("/Volumes/External/.Trashes/501/old.dmg", 60)])];
    let plan = plan_dry_run(&findings, &Selection::everything(), session(), None)
        .unwrap_or_else(|error| panic!("{error}"));
    let request = WriteRequest { max_risk: Some(Risk::Amber), ..applying() };
    let verdict = approve(plan, &request, &mount_table(), &fs());
    assert!(matches!(verdict, Ok(Verdict::NeedsConfirmation(_))), "{verdict:?}");
}

#[test]
fn an_excluded_path_never_reaches_the_executor() {
    let exclusions =
        Exclusions::new(["/Users/dana/Library/Caches/*.cache"]).unwrap_or_else(|e| panic!("{e}"));
    let rejection = rejection(&[(CACHE, 10)], &WriteRequest { exclusions, ..applying() });
    assert_eq!(rejection, GuardRejection::Excluded(CACHE.into()));
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

#[test]
fn a_plan_larger_than_max_size_is_rejected() {
    let request = WriteRequest { max_size: Some(25), ..applying() };
    let rejection = rejection(&[(CACHE, 10), ("/Users/dana/Library/Caches/other.cache", 20)], &request);
    assert_eq!(rejection, GuardRejection::MaxSizeExceeded { planned_bytes: 30, max_bytes: 25 });
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

#[test]
fn a_plan_exactly_at_max_size_is_approved() {
    let request = WriteRequest { max_size: Some(30), ..applying() };
    let verdict = approve_paths(&[(CACHE, 10), ("/Users/dana/Library/Caches/other.cache", 20)], &request);
    assert!(matches!(verdict, Ok(Verdict::NeedsConfirmation(_))), "{verdict:?}");
}

/// An `inform_only` item can only reach the guard if a caller bypasses the
/// planner, which refuses the selection outright; both doors are closed.
#[test]
fn an_inform_only_item_rejects_the_whole_plan() {
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

    let refused = plan_dry_run(std::slice::from_ref(&cloud), &Selection::everything(), session(), None);
    assert!(refused.is_err(), "the planner must refuse an inform-only selection");

    let forced = CleanPlan::dry_run(
        session(),
        vec![broza::model::CleanItem {
            path: "/Users/dana/Library/Mobile Documents/synced.key".into(),
            finding_id: cloud.id().clone(),
            size_bytes: 70,
            status: broza::model::ItemStatus::Planned,
            action: Action::InformOnly,
            error: None,
        }],
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let rejection = approve(forced, &applying(), &mount_table(), &fs())
        .err()
        .unwrap_or_else(|| panic!("an inform-only item must reject the plan"));
    assert_eq!(
        rejection,
        GuardRejection::InformOnlyItem("/Users/dana/Library/Mobile Documents/synced.key".into())
    );
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

#[test]
fn a_red_selection_is_rejected_before_any_prompt() {
    let request = WriteRequest { max_risk: Some(Risk::Red), ..applying() };
    let rejection = rejection(&[(CACHE, 10)], &request);
    assert_eq!(rejection, GuardRejection::Policy(PolicyError::Rejected(RejectReason::RedNotActionable)));
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

#[test]
fn yes_together_with_purge_is_a_usage_error() {
    let request = WriteRequest { purge: true, yes: true, ..applying() };
    let rejection = rejection(&[(CACHE, 10)], &request);
    assert_eq!(rejection, GuardRejection::Policy(PolicyError::Rejected(RejectReason::YesWithPurge)));
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

#[test]
fn without_a_terminal_and_without_yes_the_answer_cannot_be_obtained() {
    let request = WriteRequest { tty: false, ..applying() };
    let rejection = rejection(&[(CACHE, 10)], &request);
    assert_eq!(rejection, GuardRejection::Policy(PolicyError::ConfirmationRequired));
    assert_eq!(exit_code(rejection), ExitCode::ConfirmationRequired);
}

fn pending_or_panic(request: &WriteRequest) -> broza::safety::PendingApproval {
    match approve_paths(&[(CACHE, 10)], request) {
        Ok(Verdict::NeedsConfirmation(pending)) => pending,
        other => panic!("expected a pending approval, got {other:?}"),
    }
}

#[test]
fn answering_no_aborts_with_exit_six() {
    let pending = pending_or_panic(&applying());
    let prompter = FakePrompter::answering(Answer::No);
    let rejection = pending
        .confirm(&prompter)
        .err()
        .unwrap_or_else(|| panic!("a declined confirmation must not produce a token"));
    assert_eq!(rejection, GuardRejection::Policy(PolicyError::AbortedByUser));
    assert_eq!(exit_code(rejection), ExitCode::AbortedByUser);
    assert_eq!(prompter.asked(), 1);
}

#[test]
fn a_prompter_without_a_terminal_yields_exit_seven() {
    let pending = pending_or_panic(&applying());
    let rejection = pending
        .confirm(&FakePrompter::answering(Answer::NoTty))
        .err()
        .unwrap_or_else(|| panic!("no terminal means no token"));
    assert_eq!(exit_code(rejection), ExitCode::ConfirmationRequired);
}

#[test]
fn answering_yes_produces_the_token_with_the_plan_intact() {
    let pending = pending_or_panic(&applying());
    assert_eq!(pending.request().item_count, 1);
    assert_eq!(pending.request().total_bytes, 10);
    let approved =
        pending.confirm(&FakePrompter::answering(Answer::Yes)).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(approved.plan().planned_bytes(), 10);
    assert_eq!(approved.into_plan().items().len(), 1);
}

#[test]
fn yes_skips_the_prompt_entirely() {
    let request = WriteRequest { yes: true, ..applying() };
    let pending = pending_or_panic(&request);
    let prompter = FakePrompter::answering(Answer::No);
    let approved = pending.confirm(&prompter).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(approved.plan().items().len(), 1);
    assert_eq!(prompter.asked(), 0, "--yes must not reach the prompter");
}

#[test]
fn purge_demands_the_literal_word_on_a_terminal() {
    let request = WriteRequest { purge: true, ..applying() };
    let pending = pending_or_panic(&request);
    assert!(pending.request().irreversible);
    let prompter = FakePrompter::answering(Answer::Yes);
    let approved = pending.confirm(&prompter).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(approved.plan().items().len(), 1);
    assert_eq!(prompter.literal_asked(), 1, "PURGE must be typed, not answered y/N");
}

#[test]
fn a_quarantine_write_stays_inside_the_store() {
    let inside = PathBuf::from(format!("{STORE}/cln_20260921103608_a1b2/items/1/a"));
    let approved =
        approve_quarantine_write(std::slice::from_ref(&inside), Path::new(STORE), &mount_table(), &fs())
            .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(approved.paths(), [inside]);

    let outside = approve_quarantine_write(&[PathBuf::from(CACHE)], Path::new(STORE), &mount_table(), &fs());
    assert_eq!(
        outside.err(),
        Some(GuardRejection::OutsideQuarantineStore { path: CACHE.into(), store_root: STORE.into() })
    );
    let store_itself =
        approve_quarantine_write(&[PathBuf::from(STORE)], Path::new(STORE), &mount_table(), &fs());
    assert!(store_itself.is_err(), "the store root is not one of its own entries");
}

#[test]
fn the_maximum_risk_the_guard_is_given_comes_from_the_planner() {
    let findings = [
        finding("user-cache.app", Category::UserCache, &[(CACHE, 10)]),
        finding("trash.volumes", Category::Trash, &[("/Volumes/External/.Trashes/501/old.dmg", 60)]),
    ];
    assert_eq!(max_risk(&findings, &Selection::everything()), Some(Risk::Amber));
}
