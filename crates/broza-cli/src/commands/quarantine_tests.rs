//! Tests of `broza quarantine` and `broza restore` against the fake machine:
//! the round trip `clean --apply` → `list` → `restore` puts bytes back
//! unchanged, and `expire`/`purge` free what the store holds.
#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use broza::config::Config;
use broza::ports::{Answer, FileOps, Ports};
use broza::{BrozaError, ExitCode};

use crate::args::{CleanArgs, QuarantineCommand, RestoreArgs};
use crate::commands::Outcome;
use crate::commands::clean::{CleanContext, run as clean};
use crate::commands::quarantine::run as quarantine;
use crate::commands::restore::run as restore;
use crate::commands::test_world::{CACHE_CONTENTS, CACHE_FILE, folders, host, now, store_context, world};
use crate::output::{ColorPolicy, OutputFormat};

/// Quarantine the cache file with `--yes`, returning the session id.
fn cleaned(ports: &Ports) -> String {
    let config = Config::default();
    let args = CleanArgs {
        apply: true,
        categories: vec!["user-cache".into()],
        risk: None,
        purge: false,
        yes: true,
        max_size: None,
        exclude: Vec::new(),
        unused_after: None,
    };
    let outcome = clean(&CleanContext {
        ports,
        args: &args,
        config: &config,
        host: host(),
        generated_at: now(),
        warnings: Vec::new(),
        policy: ColorPolicy::Never,
        format: OutputFormat::Json,
        folders: folders(),
        tty: false,
        ci: false,
        uid_temp_dirs: Vec::new(),
    })
    .unwrap_or_else(|e| panic!("{e}"));
    let value: serde_json::Value = serde_json::from_str(&outcome.rendered).unwrap_or_else(|e| panic!("{e}"));
    value["data"]["session_id"].as_str().unwrap_or_else(|| panic!("{value}")).to_owned()
}

fn json(outcome: &Outcome) -> serde_json::Value {
    serde_json::from_str(&outcome.rendered).unwrap_or_else(|e| panic!("{e}: {}", outcome.rendered))
}

fn restore_args(ids: &[&str]) -> RestoreArgs {
    RestoreArgs {
        ids: ids.iter().map(|s| (*s).to_owned()).collect(),
        list: false,
        all: false,
        session: None,
        to: None,
    }
}

fn list(ports: &Ports, format: OutputFormat) -> Result<Outcome, BrozaError> {
    quarantine(&QuarantineCommand::List, &store_context(ports, &Config::default(), format))
}

#[test]
fn an_empty_store_lists_as_empty_in_every_format() {
    let (ports, _) = world();

    let human = list(&ports, OutputFormat::Human).unwrap_or_else(|e| panic!("{e}"));
    let csv = list(&ports, OutputFormat::Csv).unwrap_or_else(|e| panic!("{e}"));
    let value = json(&list(&ports, OutputFormat::Json).unwrap_or_else(|e| panic!("{e}")));

    assert_eq!(human.rendered, "Quarantine is empty.");
    assert_eq!(csv.rendered, "id,created_at,expires_at,total_bytes,item_count,state");
    assert_eq!(value["data"]["total_bytes"], 0);
    assert_eq!(human.code, ExitCode::Ok);
}

#[test]
fn a_cleanup_shows_up_as_one_complete_session_holding_its_bytes() {
    let (ports, _) = world();
    let session = cleaned(&ports);

    let value = json(&list(&ports, OutputFormat::Json).unwrap_or_else(|e| panic!("{e}")));
    let human = list(&ports, OutputFormat::Human).unwrap_or_else(|e| panic!("{e}")).rendered;

    assert_eq!(value["data"]["sessions"][0]["id"], session.as_str());
    assert_eq!(value["data"]["sessions"][0]["state"], "complete");
    assert!(value["data"]["total_bytes"].as_u64().unwrap_or(0) > 0);
    assert!(human.contains(&session) && human.contains("complete"), "{human}");
}

#[test]
fn restoring_the_session_puts_the_file_back_byte_for_byte_and_empties_the_store() {
    let (ports, handles) = world();
    let session = cleaned(&ports);
    assert!(!handles.fs.exists(Path::new(CACHE_FILE)));
    let config = Config::default();
    let listed = restore(
        &RestoreArgs { list: true, ..restore_args(&[]) },
        &store_context(&ports, &config, OutputFormat::Csv),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let cache_dir = Path::new(CACHE_FILE).parent().unwrap_or_else(|| panic!("parent")).display().to_string();
    assert!(listed.rendered.contains(&format!("{session}/0001,{cache_dir},")), "{}", listed.rendered);

    let outcome = restore(
        &RestoreArgs { session: Some(session.clone()), ..restore_args(&[]) },
        &store_context(&ports, &config, OutputFormat::Json),
    )
    .unwrap_or_else(|e| panic!("{e}"));

    let value = json(&outcome);
    assert_eq!(value["data"]["sessions"][0]["status"], "restored", "{value}");
    assert!(value["data"]["restored_bytes"].as_u64().unwrap_or(0) > 0);
    assert_eq!(outcome.code, ExitCode::Ok);
    assert_eq!(handles.fs.read(Path::new(CACHE_FILE)).ok().as_deref(), Some(CACHE_CONTENTS));
    let after = json(&list(&ports, OutputFormat::Json).unwrap_or_else(|e| panic!("{e}")));
    assert_eq!(after["data"]["sessions"].as_array().map(Vec::len), Some(0), "{after}");
}

#[test]
fn a_single_item_can_be_restored_somewhere_else() {
    let (ports, handles) = world();
    let session = cleaned(&ports);
    let config = Config::default();
    handles.fs.add_dir("/System/Volumes/Data/Users/dana/Restored");

    let outcome = restore(
        &RestoreArgs {
            to: Some(PathBuf::from("/System/Volumes/Data/Users/dana/Restored")),
            ..restore_args(&[&format!("{session}/0001")])
        },
        &store_context(&ports, &config, OutputFormat::Json),
    )
    .unwrap_or_else(|e| panic!("{e}"));

    let value = json(&outcome);
    let restored_to = value["data"]["sessions"][0]["items"][0]["restored_to"].as_str().unwrap_or_default();
    assert!(restored_to.starts_with("/System/Volumes/Data/Users/dana/Restored/"), "{value}");
    assert!(!handles.fs.exists(Path::new(CACHE_FILE)), "the original path is untouched");
}

#[test]
fn restore_refuses_a_mix_of_session_and_item_ids_and_names_an_unknown_session() {
    let (ports, _) = world();
    let session = cleaned(&ports);
    let config = Config::default();

    let mixed = restore(
        &restore_args(&[&session, &format!("{session}/0001")]),
        &store_context(&ports, &config, OutputFormat::Human),
    )
    .expect_err("mixed ids");
    let unknown = restore(
        &restore_args(&["cln_20200101000000_zzzz"]),
        &store_context(&ports, &config, OutputFormat::Human),
    )
    .expect_err("unknown");
    let malformed = restore(&restore_args(&["nope"]), &store_context(&ports, &config, OutputFormat::Human))
        .expect_err("bad id");

    assert_eq!(ExitCode::from(&mixed), ExitCode::UsageError);
    assert_eq!(ExitCode::from(&unknown), ExitCode::TargetNotFound);
    assert_eq!(ExitCode::from(&malformed), ExitCode::UsageError);
}

#[test]
fn expire_does_nothing_before_the_retention_and_frees_the_session_after_it() {
    let (ports, handles) = world();
    cleaned(&ports);
    let config = Config::default();

    let early = quarantine(
        &QuarantineCommand::Expire { yes: true },
        &store_context(&ports, &config, OutputFormat::Human),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    handles.clock.advance(config.quarantine_ttl.to_duration() + Duration::from_secs(86_400));
    handles.prompter.queue(&[Answer::NoTty]);
    let unattended = quarantine(
        &QuarantineCommand::Expire { yes: false },
        &store_context(&ports, &config, OutputFormat::Human),
    )
    .expect_err("no terminal");
    let late = quarantine(
        &QuarantineCommand::Expire { yes: true },
        &store_context(&ports, &config, OutputFormat::Json),
    )
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(early.rendered, "Nothing to expire.");
    assert_eq!(ExitCode::from(&unattended), ExitCode::ConfirmationRequired);
    let value = json(&late);
    assert_eq!(value["data"]["operation"], "expire");
    assert!(value["data"]["reclaimed_bytes"].as_u64().unwrap_or(0) > 0, "{value}");
    assert_eq!(value["data"]["sessions"][0]["status"], "purged");
    assert_eq!(late.code, ExitCode::Ok);
}

#[test]
fn purge_needs_the_typed_word_refuses_unknown_sessions_and_then_removes_them() {
    let (ports, handles) = world();
    let session = cleaned(&ports);
    let config = Config::default();
    let named = |ids: Vec<String>| QuarantineCommand::Purge { sessions: ids, all: false, yes: false };

    let unknown = quarantine(
        &named(vec!["cln_20200101000000_zzzz".into()]),
        &store_context(&ports, &config, OutputFormat::Human),
    )
    .expect_err("unknown");
    handles.prompter.queue(&[Answer::No]);
    let declined =
        quarantine(&named(vec![session.clone()]), &store_context(&ports, &config, OutputFormat::Human))
            .expect_err("declined");
    handles.prompter.queue(&[Answer::Yes]);
    let purged =
        quarantine(&named(vec![session.clone()]), &store_context(&ports, &config, OutputFormat::Json))
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(ExitCode::from(&unknown), ExitCode::TargetNotFound);
    assert_eq!(ExitCode::from(&declined), ExitCode::AbortedByUser);
    assert_eq!(
        handles.prompter.prompts().last().and_then(|p| p.expected_literal.clone()).as_deref(),
        Some("PURGE")
    );
    let value = json(&purged);
    assert_eq!(value["data"]["operation"], "purge");
    assert_eq!(value["data"]["sessions"][0]["id"], session.as_str());
    assert_eq!(value["data"]["sessions"][0]["status"], "purged");
    assert!(
        json(&list(&ports, OutputFormat::Json).unwrap_or_else(|e| panic!("{e}")))["data"]["sessions"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
}

#[test]
fn purge_without_a_terminal_exits_seven_and_removes_nothing() {
    let (ports, handles) = world();
    let session = cleaned(&ports);
    let config = Config::default();
    handles.prompter.queue(&[Answer::NoTty]);

    let error = quarantine(
        &QuarantineCommand::Purge { sessions: vec![session.clone()], all: false, yes: false },
        &store_context(&ports, &config, OutputFormat::Human),
    )
    .expect_err("no terminal");

    assert_eq!(ExitCode::from(&error), ExitCode::ConfirmationRequired);
    let listed = json(&list(&ports, OutputFormat::Json).unwrap_or_else(|e| panic!("{e}")));
    assert_eq!(listed["data"]["sessions"][0]["id"], session.as_str(), "still there");
}

#[test]
fn a_restore_naming_an_unknown_session_beside_a_known_one_writes_nothing_and_exits_four() {
    let (ports, handles) = world();
    let session = cleaned(&ports);
    let config = Config::default();

    let error = restore(
        &restore_args(&[&session, "cln_20200101000000_zzzz"]),
        &store_context(&ports, &config, OutputFormat::Json),
    )
    .expect_err("unknown session");

    assert_eq!(ExitCode::from(&error), ExitCode::TargetNotFound);
    assert!(!handles.fs.exists(Path::new(CACHE_FILE)), "the known session was not restored either");
}

#[test]
fn a_restore_naming_an_item_the_session_does_not_hold_exits_four() {
    let (ports, handles) = world();
    let session = cleaned(&ports);
    let config = Config::default();

    let error = restore(
        &restore_args(&[&format!("{session}/9999")]),
        &store_context(&ports, &config, OutputFormat::Json),
    )
    .expect_err("unknown item");

    assert_eq!(ExitCode::from(&error), ExitCode::TargetNotFound);
    assert!(error.to_string().contains("9999"), "{error}");
    assert!(!handles.fs.exists(Path::new(CACHE_FILE)));
}

#[test]
fn restore_all_reports_a_session_it_cannot_read_and_still_restores_the_rest() {
    let (ports, handles) = world();
    cleaned(&ports);
    let config = Config::default();
    let corrupt = format!("{}/cln_20200101000000_bbbb", crate::commands::test_world::STORE);
    handles.fs.add_file(format!("{corrupt}/manifest.json"), b"{ not json");

    let outcome = restore(
        &RestoreArgs { all: true, ..restore_args(&[]) },
        &store_context(&ports, &config, OutputFormat::Json),
    )
    .unwrap_or_else(|e| panic!("{e}"));

    let value = json(&outcome);
    assert_eq!(outcome.code, ExitCode::PartialFailure, "{value}");
    assert!(value["errors"].as_array().is_some_and(|errors| !errors.is_empty()), "{value}");
    assert_eq!(
        handles.fs.read(Path::new(CACHE_FILE)).ok().as_deref(),
        Some(CACHE_CONTENTS),
        "the good session came back"
    );
}

#[test]
fn restore_all_on_an_empty_store_says_so() {
    let (ports, _) = world();

    let outcome = restore(
        &RestoreArgs { all: true, ..restore_args(&[]) },
        &store_context(&ports, &Config::default(), OutputFormat::Human),
    )
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(outcome.rendered, "Quarantine is empty.");
    assert_eq!(outcome.code, ExitCode::Ok);
}
