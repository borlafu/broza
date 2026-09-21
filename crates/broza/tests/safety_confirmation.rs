//! Behavioural tests of the confirmation half of the safety kernel.
//!
//! The dry run, the empty plan, the four confirmation modes, the exit codes `6`
//! and `7`, and the quarantine-store approval. Only the `test-support` feature
//! exposes `broza::testing`: without it this file compiles to nothing.
#![cfg(feature = "test-support")]

mod scenario;

use std::path::{Path, PathBuf};

use broza::ExitCode;
use broza::clean::{Selection, max_risk};
use broza::model::{Action, Category, CleanPlan, Risk};
use broza::ports::Answer;
use broza::safety::guard::{Verdict, WriteRequest, approve, approve_quarantine_write};
use broza::safety::rejection::{GuardRejection, PolicyError};
use broza::testing::FakePrompter;

use scenario::{
    CACHE, HOME, STORE, applying, approve_paths, caches, exit_code, finding, fs, mounts, pending_or_panic,
    planned, rejection, session,
};

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
fn an_empty_plan_is_nothing_to_do_even_without_a_terminal() {
    let request = WriteRequest { tty: false, ..applying() };
    let plan = CleanPlan::dry_run(session(), Vec::new()).unwrap_or_else(|error| panic!("{error}"));
    let verdict = approve(plan, &[], &request, &mounts(), &fs());
    assert!(matches!(verdict, Ok(Verdict::Nothing)), "{verdict:?}");
}

#[test]
fn yes_together_with_purge_is_a_usage_error() {
    let request = WriteRequest { purge: true, yes: true, ..applying() };
    let rejection = rejection(&[(CACHE, 10)], &request);
    assert_eq!(
        rejection,
        GuardRejection::Policy(PolicyError::Rejected(broza::safety::RejectReason::YesWithPurge))
    );
    assert_eq!(exit_code(rejection), ExitCode::UsageError);
}

#[test]
fn without_a_terminal_and_without_yes_the_answer_cannot_be_obtained() {
    let request = WriteRequest { tty: false, ..applying() };
    let rejection = rejection(&[(CACHE, 10)], &request);
    assert_eq!(rejection, GuardRejection::Policy(PolicyError::ConfirmationRequired));
    assert_eq!(exit_code(rejection), ExitCode::ConfirmationRequired);
}

#[test]
fn ci_is_treated_as_no_terminal() {
    let request = WriteRequest { ci: true, ..applying() };
    let rejection = rejection(&[(CACHE, 10)], &request);
    assert_eq!(exit_code(rejection), ExitCode::ConfirmationRequired);
}

#[test]
fn answering_no_aborts_with_exit_six() {
    let prompter = FakePrompter::always(Answer::No);
    let rejection = pending_or_panic(&applying())
        .confirm(&prompter)
        .err()
        .unwrap_or_else(|| panic!("a declined confirmation must not produce a token"));
    assert_eq!(rejection, GuardRejection::Policy(PolicyError::AbortedByUser));
    assert_eq!(exit_code(rejection), ExitCode::AbortedByUser);
    assert_eq!(prompter.prompts().len(), 1);
}

#[test]
fn a_prompter_without_a_terminal_yields_exit_seven() {
    let rejection = pending_or_panic(&applying())
        .confirm(&FakePrompter::always(Answer::NoTty))
        .err()
        .unwrap_or_else(|| panic!("no terminal means no token"));
    assert_eq!(exit_code(rejection), ExitCode::ConfirmationRequired);
}

#[test]
fn yes_skips_the_prompt_entirely() {
    let request = WriteRequest { yes: true, ..applying() };
    let prompter = FakePrompter::always(Answer::No);
    let approved = pending_or_panic(&request).confirm(&prompter).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(approved.plan().items().len(), 1);
    assert!(prompter.prompts().is_empty(), "--yes must not reach the prompter");
}

#[test]
fn an_amber_plan_is_shown_in_full_before_the_question() {
    let findings =
        vec![finding("trash.volumes", Category::Trash, &[("/Volumes/External/.Trashes/501/old.dmg", 60)])];
    let plan = planned(&findings, &Selection::everything()).plan;
    let pending = match approve(plan, &findings, &applying(), &mounts(), &fs()) {
        Ok(Verdict::NeedsConfirmation(pending)) => pending,
        other => panic!("expected a pending approval, got {other:?}"),
    };
    assert_eq!(pending.mode(), broza::safety::ConfirmationMode::DetailedExplicit);
    assert_eq!(pending.request().max_risk, Risk::Amber);
    assert_eq!(pending.request().preview.len(), 1);
}

#[test]
fn a_green_plan_only_needs_a_simple_question() {
    let pending = pending_or_panic(&applying());
    assert_eq!(pending.mode(), broza::safety::ConfirmationMode::SimpleYesNo);
    assert_eq!(pending.request().max_risk, Risk::Green);
    assert!(!pending.request().irreversible);
}

#[test]
fn purge_demands_the_literal_word_on_a_terminal() {
    let findings = caches(&[(CACHE, 10)]);
    let purging = Selection { purge: true, ..Selection::everything() };
    let plan = planned(&findings, &purging).plan;
    let request = WriteRequest { purge: true, ..applying() };
    let pending = match approve(plan, &findings, &request, &mounts(), &fs()) {
        Ok(Verdict::NeedsConfirmation(pending)) => pending,
        other => panic!("expected a pending approval, got {other:?}"),
    };
    assert!(pending.request().irreversible);
    assert_eq!(pending.mode(), broza::safety::ConfirmationMode::TypedLiteral(broza::safety::PURGE_LITERAL));
    let prompter = FakePrompter::always(Answer::Yes);
    let approved = pending.confirm(&prompter).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(approved.plan().items()[0].action, Action::Purge);
    assert_eq!(prompter.literals(), ["PURGE"], "PURGE must be typed, not answered y/N");
}

#[test]
fn a_quarantine_write_stays_inside_the_store() {
    let inside = PathBuf::from(format!("{STORE}/cln_20260921103608_a1b2/items/1/a"));
    let approved =
        approve_quarantine_write(std::slice::from_ref(&inside), Path::new(STORE), &mounts(), &fs())
            .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(approved.items()[0].path, inside);
    assert!(approved.items()[0].inode > 0, "the executor re-checks the identity");

    let outside = approve_quarantine_write(&[PathBuf::from(CACHE)], Path::new(STORE), &mounts(), &fs());
    assert_eq!(
        outside.err(),
        Some(GuardRejection::OutsideQuarantineStore { path: CACHE.into(), store_root: STORE.into() })
    );
    let store_itself = approve_quarantine_write(&[PathBuf::from(STORE)], Path::new(STORE), &mounts(), &fs());
    assert!(store_itself.is_err(), "the store root is not one of its own entries");
}

#[test]
fn a_missing_quarantine_entry_is_reported_as_not_found() {
    let gone = PathBuf::from(format!("{STORE}/cln_20260921103608_a1b2/items/9/gone"));
    let rejection = approve_quarantine_write(&[gone], Path::new(STORE), &mounts(), &fs())
        .err()
        .unwrap_or_else(|| panic!("a missing entry must be reported"));
    assert_eq!(exit_code(rejection), ExitCode::TargetNotFound);
}

#[test]
fn the_maximum_risk_of_a_selection_is_reported_for_the_summary() {
    let findings = [
        finding("user-cache.app", Category::UserCache, &[(CACHE, 10)]),
        finding("trash.volumes", Category::Trash, &[("/Volumes/External/.Trashes/501/old.dmg", 60)]),
    ];
    assert_eq!(max_risk(&findings, &Selection::everything()), Some(Risk::Amber));
}
