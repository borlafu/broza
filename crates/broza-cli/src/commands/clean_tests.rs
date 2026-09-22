//! Tests of `broza clean` against the fake ports: dry run, the confirmation
//! matrix, an applied run, and the pre-execution expiry.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::time::Duration;

use broza::config::Config;
use broza::ports::{Answer, FileOps};
use broza::{BrozaError, ExitCode};

use crate::args::CleanArgs;
use crate::commands::Outcome;
use crate::commands::clean::{CleanContext, Executed, INFORM_ONLY_SKIPPED_CODE, render, run};
use crate::commands::clean_expiry::{EXPIRY_DECLINED_CODE, EXPIRY_PENDING_CODE, EXPIRY_UNREADABLE_CODE};
use crate::commands::test_world::{CACHE_FILE, STORE, folders, host, now, world};
use crate::output::OutputFormat;
use broza::model::{CleanPlan, ItemErrorCode, ItemStatus};
use broza::ports::Ports;

fn args() -> CleanArgs {
    CleanArgs {
        apply: false,
        categories: vec!["user-cache".into()],
        risk: None,
        purge: false,
        yes: false,
        max_size: None,
        exclude: Vec::new(),
        unused_after: None,
    }
}

fn context<'a>(
    ports: &'a Ports,
    args: &'a CleanArgs,
    config: &'a Config,
    tty: bool,
    format: OutputFormat,
) -> CleanContext<'a> {
    CleanContext {
        ports,
        args,
        config,
        host: host(),
        generated_at: now(),
        warnings: Vec::new(),
        format,
        folders: folders(),
        tty,
        ci: false,
        uid_temp_dirs: Vec::new(),
    }
}

fn run_with(ports: &Ports, args: &CleanArgs, tty: bool, format: OutputFormat) -> Result<Outcome, BrozaError> {
    let config = Config::default();
    run(&context(ports, args, &config, tty, format))
}

fn json(outcome: &Outcome) -> serde_json::Value {
    serde_json::from_str(&outcome.rendered).unwrap_or_else(|e| panic!("{e}: {}", outcome.rendered))
}

fn past_retention(handles: &broza::testing::Handles) {
    handles.clock.advance(Config::default().quarantine_ttl.to_duration() + Duration::from_secs(86_400));
}

#[test]
fn a_dry_run_plans_the_caches_touches_nothing_and_exits_zero() {
    let (ports, handles) = world();

    let outcome = run_with(&ports, &args(), false, OutputFormat::Json).unwrap();

    let value: serde_json::Value = serde_json::from_str(&outcome.rendered).unwrap();
    assert_eq!(value["data"]["dry_run"], true);
    assert_eq!(value["data"]["items"][0]["status"], "planned");
    assert_eq!(value["data"]["quarantined_bytes"], 0);
    assert_eq!(outcome.code, ExitCode::Ok);
    assert!(handles.fs.exists(Path::new(CACHE_FILE)));
    assert!(handles.prompter.prompts().is_empty(), "a dry run never asks");
}

#[test]
fn a_selection_is_mandatory() {
    let (ports, _) = world();
    let none = CleanArgs { categories: Vec::new(), ..args() };

    let error = run_with(&ports, &none, false, OutputFormat::Human).expect_err("usage error");

    assert!(matches!(error, BrozaError::Usage(_)), "{error}");
    assert!(error.to_string().contains("--category"), "{error}");
}

#[test]
fn apply_without_a_terminal_or_yes_exits_seven() {
    let (ports, handles) = world();
    let apply = CleanArgs { apply: true, ..args() };

    let error = run_with(&ports, &apply, false, OutputFormat::Human).expect_err("no confirmation");

    assert_eq!(ExitCode::from(&error), ExitCode::ConfirmationRequired);
    assert!(handles.fs.exists(Path::new(CACHE_FILE)), "nothing moved");
}

#[test]
fn apply_with_yes_moves_the_caches_into_a_session_and_reports_pending_bytes() {
    let (ports, handles) = world();
    let apply = CleanArgs { apply: true, yes: true, ..args() };

    let outcome = run_with(&ports, &apply, false, OutputFormat::Json).unwrap();

    let value: serde_json::Value = serde_json::from_str(&outcome.rendered).unwrap();
    assert_eq!(value["data"]["dry_run"], false);
    assert_eq!(value["data"]["items"][0]["status"], "quarantined");
    assert!(value["data"]["quarantined_bytes"].as_u64().unwrap() > 0);
    assert_eq!(value["data"]["reclaimed_bytes"], 0, "quarantining frees nothing");
    assert_eq!(outcome.code, ExitCode::Ok);
    assert!(!handles.fs.exists(Path::new(CACHE_FILE)), "the cache left its place");
    let store = value["data"]["quarantine_path"].as_str().unwrap();
    assert!(store.starts_with(STORE), "{store}");
    assert!(handles.prompter.prompts().is_empty(), "--yes covers the confirmation");
}

#[test]
fn a_declined_confirmation_exits_six_and_moves_nothing() {
    let (ports, handles) = world();
    handles.prompter.queue(&[Answer::No]);
    let apply = CleanArgs { apply: true, ..args() };

    let error = run_with(&ports, &apply, true, OutputFormat::Human).expect_err("aborted");

    assert_eq!(ExitCode::from(&error), ExitCode::AbortedByUser);
    assert!(handles.fs.exists(Path::new(CACHE_FILE)));
}

#[test]
fn an_inform_only_finding_in_a_cleanable_category_is_skipped_with_a_warning() {
    let (ports, _) = world();
    let build = CleanArgs { categories: vec!["build-cache".into()], ..args() };

    let outcome = run_with(&ports, &build, false, OutputFormat::Human).unwrap();

    assert!(outcome.warnings.iter().any(|w| w.code == INFORM_ONLY_SKIPPED_CODE), "{:?}", outcome.warnings);
    assert_eq!(outcome.code, ExitCode::Ok);
}

#[test]
fn a_second_apply_expires_the_first_session_once_its_retention_is_over() {
    let (ports, handles) = world();
    let apply = CleanArgs { apply: true, yes: true, ..args() };
    run_with(&ports, &apply, false, OutputFormat::Json).unwrap();
    handles.fs.add_file(CACHE_FILE, b"cached again");
    handles.fs.set_size(CACHE_FILE, 900_000_000);
    handles.clock.advance(Config::default().quarantine_ttl.to_duration() + Duration::from_secs(86_400));

    let outcome = run_with(&ports, &apply, false, OutputFormat::Json).unwrap();

    let value: serde_json::Value = serde_json::from_str(&outcome.rendered).unwrap();
    assert_eq!(value["data"]["expired_sessions"].as_array().map(Vec::len), Some(1), "{}", outcome.rendered);
    assert!(value["data"]["reclaimed_bytes"].as_u64().unwrap() > 0, "expiry frees space");
    assert_eq!(outcome.code, ExitCode::Ok, "{:?}", outcome.warnings);
}

#[test]
fn a_dry_run_reports_sessions_past_their_retention_without_touching_them() {
    let (ports, handles) = world();
    let apply = CleanArgs { apply: true, yes: true, ..args() };
    run_with(&ports, &apply, false, OutputFormat::Json).unwrap();
    handles.fs.add_file(CACHE_FILE, b"again");
    handles.fs.set_size(CACHE_FILE, 900_000_000);
    past_retention(&handles);

    let outcome = run_with(&ports, &args(), false, OutputFormat::Human).unwrap();

    assert!(outcome.warnings.iter().any(|w| w.code == EXPIRY_PENDING_CODE), "{:?}", outcome.warnings);
    assert!(
        outcome.rendered.contains("1 quarantine session(s) past their retention"),
        "{}",
        outcome.rendered
    );
    assert_eq!(json(&run_with(&ports, &args(), false, OutputFormat::Json).unwrap())["data"]["dry_run"], true);
    assert!(handles.fs.exists(Path::new(STORE)), "the session is still there");
}

#[test]
fn a_declined_expiry_prompt_keeps_the_old_session_and_still_moves_the_new_items() {
    let (ports, handles) = world();
    let with_yes = CleanArgs { apply: true, yes: true, ..args() };
    run_with(&ports, &with_yes, false, OutputFormat::Json).unwrap();
    handles.fs.add_file(CACHE_FILE, b"again");
    handles.fs.set_size(CACHE_FILE, 900_000_000);
    past_retention(&handles);
    handles.prompter.queue(&[Answer::Yes, Answer::No]);
    let prompted = CleanArgs { apply: true, ..args() };

    let outcome = run_with(&ports, &prompted, true, OutputFormat::Json).unwrap();

    let value = json(&outcome);
    assert!(outcome.warnings.iter().any(|w| w.code == EXPIRY_DECLINED_CODE), "{:?}", outcome.warnings);
    assert_eq!(value["data"]["items"][0]["status"], "quarantined", "{value}");
    assert!(value["data"]["expired_sessions"].is_null(), "{value}");
    assert_eq!(value["data"]["reclaimed_bytes"], 0);
    assert_eq!(handles.prompter.prompts().len(), 2, "plan, then expiry");
    assert_eq!(outcome.code, ExitCode::Ok);
}

#[test]
fn an_unreadable_store_costs_the_expiry_step_and_not_the_dry_run() {
    let (ports, handles) = world();
    // A session directory Broza may not even `stat`: reading the store fails.
    let unreadable = format!("{STORE}/cln_20200101000000_aaaa");
    handles.fs.add_dir(&unreadable);
    handles.fs.add_denied(&unreadable);

    let outcome = run_with(&ports, &args(), false, OutputFormat::Human).unwrap();

    assert!(outcome.warnings.iter().any(|w| w.code == EXPIRY_UNREADABLE_CODE), "{:?}", outcome.warnings);
    assert!(outcome.rendered.starts_with("Dry run"), "{}", outcome.rendered);
    assert_eq!(outcome.code, ExitCode::Ok);
}

#[test]
fn apply_with_nothing_to_move_still_expires_what_is_due_and_creates_no_session() {
    let (ports, handles) = world();
    run_with(&ports, &CleanArgs { apply: true, yes: true, ..args() }, false, OutputFormat::Json).unwrap();
    past_retention(&handles);
    // Only the inform-only Docker disk is left in build-cache: nothing to move.
    let build = CleanArgs { apply: true, yes: true, categories: vec!["build-cache".into()], ..args() };

    let outcome = run_with(&ports, &build, false, OutputFormat::Json).unwrap();
    let human = run_with(&ports, &build, false, OutputFormat::Human).unwrap();

    let value = json(&outcome);
    assert_eq!(value["data"]["expired_sessions"].as_array().map(Vec::len), Some(1), "{value}");
    assert!(value["data"]["reclaimed_bytes"].as_u64().unwrap() > 0);
    assert!(value["data"]["quarantine_path"].is_null(), "no session was created");
    assert_eq!(outcome.code, ExitCode::Ok);
    assert!(human.rendered.starts_with("Nothing to move:"), "{}", human.rendered);
    assert!(!human.rendered.contains("Undo"), "{}", human.rendered);
}

#[test]
fn a_risk_ceiling_selects_without_naming_categories() {
    let (ports, _) = world();
    let green = CleanArgs { categories: Vec::new(), risk: Some(crate::args::RiskLevel::Green), ..args() };

    let outcome = run_with(&ports, &green, false, OutputFormat::Json).unwrap();

    let value = json(&outcome);
    assert!(!value["data"]["items"].as_array().unwrap().is_empty(), "{value}");
    assert!(outcome.rendered.contains("user-cache"), "{}", outcome.rendered);
}

#[test]
fn apply_with_purge_deletes_for_good_after_the_typed_word_and_creates_no_session() {
    let (ports, handles) = world();
    handles.prompter.queue(&[Answer::Yes]);
    let purge = CleanArgs { apply: true, purge: true, ..args() };

    let outcome = run_with(&ports, &purge, true, OutputFormat::Json).unwrap();

    let value = json(&outcome);
    assert_eq!(value["data"]["items"][0]["status"], "purged", "{value}");
    assert_eq!(value["data"]["items"][0]["action"], "purge", "{value}");
    assert!(value["data"]["reclaimed_bytes"].as_u64().unwrap() > 0, "{value}");
    assert_eq!(value["data"]["quarantined_bytes"], 0);
    assert!(value["data"]["quarantine_path"].is_null(), "no session for a purge");
    assert_eq!(handles.prompter.prompts()[0].expected_literal.as_deref(), Some("PURGE"));
    assert!(!handles.fs.exists(Path::new(CACHE_FILE)), "gone for good");
    assert!(
        !handles.fs.exists(Path::new(STORE))
            || handles.fs.read_dir(Path::new(STORE)).map_or(true, |d| d.is_empty())
    );
    assert_eq!(outcome.code, ExitCode::Ok);
}

#[test]
fn apply_with_purge_without_a_terminal_exits_seven_and_deletes_nothing() {
    let (ports, handles) = world();
    let purge = CleanArgs { apply: true, purge: true, ..args() };

    let error = run_with(&ports, &purge, false, OutputFormat::Human).expect_err("no terminal");

    assert_eq!(ExitCode::from(&error), ExitCode::ConfirmationRequired);
    assert!(handles.fs.exists(Path::new(CACHE_FILE)));
}

#[test]
fn an_item_that_was_not_moved_is_an_error_entry_and_exit_five() {
    let (ports, _) = world();
    let config = Config::default();
    let apply = CleanArgs { apply: true, yes: true, ..args() };
    let plan = CleanPlan::dry_run(
        "cln_20260921103608_a1b2".parse().unwrap(),
        vec![broza::model::CleanItem {
            path: "/Volumes/Ext/old/node_modules".into(),
            finding_id: "build-cache.orphan-node-modules".parse().unwrap(),
            size_bytes: 5,
            status: ItemStatus::Planned,
            action: broza::model::Action::Quarantine,
            error: None,
        }],
    )
    .unwrap()
    .into_applied(None)
    .unwrap()
    .with_item_status(0, ItemStatus::Skipped, Some(ItemErrorCode::CrossVolume))
    .unwrap();
    let executed = Executed {
        errors: crate::commands::clean::item_errors(&plan),
        plan,
        warnings: Vec::new(),
        due: Vec::new(),
    };

    let outcome = render(
        &context(&ports, &apply, &config, false, OutputFormat::Json),
        executed,
        Vec::new(),
        Path::new("/h"),
    )
    .unwrap();

    let value = json(&outcome);
    assert_eq!(outcome.code, ExitCode::PartialFailure);
    assert_eq!(value["errors"][0]["code"], "cross_volume", "{value}");
    assert!(value["errors"][0]["message"].as_str().unwrap().contains("(skipped)"), "{value}");
}
