//! `broza quarantine list | expire | purge` (`docs/cli-spec.md` §3.8, §4.5, §4.6).
//!
//! This is where space is actually freed: `clean --apply` only moves items into
//! the store. `expire` removes what is past its retention after a `y/N`;
//! `purge` removes whatever is named after the typed word `PURGE`, and never
//! takes `--yes`. Both write through a token the guard issued for exactly the
//! sessions they were asked about.

use broza::BrozaError;
use broza::model::{OperationKind, ReclaimReport, SessionId};
use broza::quarantine::{all_sessions, expire, expired_sessions, list_sessions, purge, store};

use crate::args::QuarantineCommand;
use crate::commands::store::{
    StoreContext, StoreOutput, confirm_purge, confirm_removal, finish, mounts, parse_session_id, root_of,
    session_dirs_token, session_sizes,
};
use crate::commands::{Outcome, Reclaimed};
use crate::output::csv;
use crate::output::human::quarantine as human;

/// Header of `quarantine list --csv` (`docs/cli-spec.md` §3.8.1).
const LIST_CSV_HEADER: [&str; 6] = ["id", "created_at", "expires_at", "total_bytes", "item_count", "state"];

/// Run one `quarantine` subcommand.
///
/// # Errors
///
/// `HOME` unset or an identifier that is not a session id (exit `2`); an
/// unknown session (exit `4`); no terminal for a confirmation (exit `7`); a
/// declined confirmation (exit `6`); whatever the guard or the store refuse.
pub fn run(command: &QuarantineCommand, context: &StoreContext<'_>) -> Result<Outcome, BrozaError> {
    match command {
        QuarantineCommand::List => list(context),
        QuarantineCommand::Expire { yes } => expire_due(context, *yes),
        QuarantineCommand::Purge { sessions, all, .. } => purge_named(context, sessions, *all),
    }
}

/// `quarantine list`: every session, with the derived `expired` state.
fn list(context: &StoreContext<'_>) -> Result<Outcome, BrozaError> {
    let root = root_of(context.config, context.home()?);
    let listed =
        list_sessions(&root, context.ports.fs.as_ref(), context.ports.clock.as_ref(), context.ttl())?;
    let rows = listed.data.sessions.iter().map(list_row);
    let table = std::iter::once(csv::row(&LIST_CSV_HEADER)).chain(rows).collect::<Vec<_>>().join("\n");
    finish(
        context,
        &StoreOutput {
            command: "quarantine list",
            human: human::render_list(&listed.data),
            csv: Some(table),
            data: listed.data,
            errors: listed.errors,
            warnings: [context.warnings.clone(), listed.warnings].concat(),
            host: context.host.clone(),
            generated_at: context.generated_at,
        },
    )
}

/// One `quarantine list --csv` row, in the contract's tokens.
fn list_row(session: &broza::model::QuarantineSession) -> String {
    csv::row(&[
        session.id.to_string(),
        session.created_at.to_string(),
        session.expires_at.to_string(),
        session.total_bytes.to_string(),
        session.item_count.to_string(),
        state_token(&session.state),
    ])
}

/// The serde token of a session state.
fn state_token(state: &broza::model::SessionState) -> String {
    serde_json::to_value(state).ok().and_then(|v| v.as_str().map(ToOwned::to_owned)).unwrap_or_default()
}

/// `quarantine expire [--yes]`: remove what is past its retention.
fn expire_due(context: &StoreContext<'_>, yes: bool) -> Result<Outcome, BrozaError> {
    let root = root_of(context.config, context.home()?);
    let ports = context.ports;
    let due = expired_sessions(&root, ports.fs.as_ref(), ports.clock.as_ref(), context.ttl())?;
    if due.is_empty() {
        return reclaimed(context, nothing(OperationKind::Expire), Vec::new(), Vec::new());
    }
    let sizes = session_sizes(ports, &root, &due)?;
    confirm_removal(ports, &sizes, yes, "expire")?;
    let (mounts, mount_warnings) = mounts(ports)?;
    let token = session_dirs_token(ports, &root, &mounts, &due)?;
    let reported = expire(&token, &due, &root, ports.fs.as_ref())?;
    reclaimed(context, reported.data, reported.errors, [mount_warnings, reported.warnings].concat())
}

/// `quarantine purge <ids> | --all`: remove sessions whatever their age, after `PURGE`.
fn purge_named(context: &StoreContext<'_>, raw_ids: &[String], all: bool) -> Result<Outcome, BrozaError> {
    let root = root_of(context.config, context.home()?);
    let ports = context.ports;
    let ids: Vec<SessionId> = if all {
        all_sessions(&root, ports.fs.as_ref())?
    } else {
        raw_ids.iter().map(|raw| parse_session_id(raw)).collect::<Result<_, _>>()?
    };
    if ids.is_empty() {
        return reclaimed(context, nothing(OperationKind::Purge), Vec::new(), Vec::new());
    }
    // Unknown identifiers are refused before the prompt: nothing to type PURGE for.
    let sizes = session_sizes(ports, &root, &ids)?;
    confirm_purge(ports, &sizes)?;
    let (mounts, mount_warnings) = mounts(ports)?;
    let token = session_dirs_token(ports, &root, &mounts, &ids)?;
    let reported = purge(&token, &ids, &root, ports.fs.as_ref())?;
    reclaimed(context, reported.data, reported.errors, [mount_warnings, reported.warnings].concat())
}

/// The report of an operation that found nothing to remove.
fn nothing(operation: OperationKind) -> ReclaimReport {
    ReclaimReport { operation, reclaimed_bytes: 0, sessions: Vec::new() }
}

/// Render a reclaim report as the command's outcome.
fn reclaimed(
    context: &StoreContext<'_>,
    data: ReclaimReport,
    errors: Vec<broza::model::Warning>,
    warnings: Vec<broza::model::Warning>,
) -> Result<Outcome, BrozaError> {
    let freed = Reclaimed { quarantined_bytes: 0, freed_bytes: data.reclaimed_bytes };
    let outcome = finish(
        context,
        &StoreOutput {
            command: match data.operation {
                OperationKind::Expire => "quarantine expire",
                _ => "quarantine purge",
            },
            human: human::render_reclaim(&data, &errors),
            csv: None,
            data,
            errors,
            warnings: [context.warnings.clone(), warnings].concat(),
            host: context.host.clone(),
            generated_at: context.generated_at,
        },
    )?;
    Ok(outcome.with_reclaimed(freed))
}

/// The identifiers the store holds, for `restore --all` and tests.
pub fn stored_ids(context: &StoreContext<'_>) -> Result<Vec<SessionId>, BrozaError> {
    let root = root_of(context.config, context.home()?);
    store::all_ids(context.ports.fs.as_ref(), &root)
}

#[cfg(test)]
mod unit {
    use broza::model::QuarantineList;

    use super::*;

    #[test]
    fn the_empty_report_of_an_operation_frees_nothing() {
        let report = nothing(OperationKind::Expire);
        assert_eq!(report.reclaimed_bytes, 0);
        assert!(report.sessions.is_empty());
    }

    #[test]
    fn list_rows_use_the_contract_tokens() {
        let listed: QuarantineList = serde_json::from_value(serde_json::json!({
            "quarantine_path": "/q",
            "total_bytes": 5,
            "expired_bytes": 0,
            "sessions": [{
                "id": "cln_20260917103608_a1b2",
                "created_at": "2026-09-17T10:36:08Z",
                "expires_at": "2026-10-17T10:36:08Z",
                "total_bytes": 5,
                "item_count": 1,
                "state": "complete"
            }]
        }))
        .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(
            list_row(&listed.sessions[0]),
            "cln_20260917103608_a1b2,2026-09-17T10:36:08Z,2026-10-17T10:36:08Z,5,1,complete"
        );
    }
}
