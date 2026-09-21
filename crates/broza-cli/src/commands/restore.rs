//! `broza restore` (`docs/cli-spec.md` §3.5 and §4.6).
//!
//! Items go back to where they came from, or under `--to`. Two tokens authorise
//! every restore: one over the stored items (they leave the store) and one over
//! the destinations (the guard checks each against the allowlist, `--to`
//! included). `--list` reads the store and writes nothing.

use std::path::Path;

use broza::BrozaError;
use broza::model::{
    EntryId, ItemStatus, OperationKind, QuarantineEntry, RestoreReport, RestoreSession, SessionId,
};
use broza::quarantine::{
    Reported, entry_destinations, restore_entries, restore_session, session_destinations, store,
};
use broza::safety::guard::{RestoreRequest, approve_restore_targets};
use broza::scan::MountTable;

use crate::args::RestoreArgs;
use crate::commands::Outcome;
use crate::commands::store::{
    StoreContext, StoreOutput, finish, mounts, parse_session_id, root_of, session_write_token,
};
use crate::output::csv;
use crate::output::human::restore as human;

/// Header of `restore --list --csv` (`docs/cli-spec.md` §3.5).
const LIST_CSV_HEADER: [&str; 4] = ["id", "original_path", "size_bytes", "status"];

/// What the identifiers on the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Wanted {
    /// Whole sessions.
    Sessions(Vec<SessionId>),
    /// Single items, possibly of several sessions.
    Entries(Vec<EntryId>),
}

/// Run `restore`.
///
/// # Errors
///
/// `HOME` unset or an identifier of neither form (exit `2`); an unknown session
/// (exit `4`); whatever the guard refuses about a destination; and whatever the
/// store reports as fatal. An item that cannot go back is an `errors[]` entry
/// and exit `5`, never an abort.
pub fn run(args: &RestoreArgs, context: &StoreContext<'_>) -> Result<Outcome, BrozaError> {
    let home = context.home()?;
    let root = root_of(context.config, home);
    if args.list {
        return list(context, &root);
    }
    let wanted = wanted_of(args, context, &root)?;
    let (mounts, mount_warnings) = mounts(context.ports)?;
    let reported = match wanted {
        Wanted::Sessions(ids) => restore_sessions(context, &root, &mounts, &ids, args.to.as_deref())?,
        Wanted::Entries(ids) => {
            let targets = target_token(context, &root, &mounts, &ids, args.to.as_deref())?;
            let sessions: Vec<SessionId> =
                ids.iter().map(|id| id.session_part().parse()).collect::<Result<_, _>>()?;
            let sources = session_write_token(context.ports, &root, &mounts, &sessions)?;
            restore_entries(&sources, &targets, &ids, &root, context.ports.fs.as_ref(), args.to.as_deref())?
        }
    };
    finish(
        context,
        &StoreOutput {
            command: "restore",
            human: human::render_restore(&reported.data, &reported.errors, home),
            csv: None,
            data: reported.data,
            errors: reported.errors,
            warnings: [context.warnings.clone(), mount_warnings, reported.warnings].concat(),
            host: context.host.clone(),
            generated_at: context.generated_at,
        },
    )
}

/// `--list`: the store's contents as a restore report of `planned` items.
fn list(context: &StoreContext<'_>, root: &Path) -> Result<Outcome, BrozaError> {
    let found = store::read_all(context.ports.fs.as_ref(), root)?;
    let sessions: Vec<RestoreSession> = found
        .sessions
        .iter()
        .map(|stored| RestoreSession {
            id: stored.id.clone(),
            status: ItemStatus::Planned,
            items: stored
                .session()
                .entries
                .iter()
                .filter(|entry| entry.stored_path.is_some())
                .map(planned)
                .collect(),
        })
        .collect();
    let data = RestoreReport { operation: OperationKind::Restore, restored_bytes: 0, sessions };
    let rows = data.sessions.iter().flat_map(|session| session.items.iter()).map(list_row);
    let table = std::iter::once(csv::row(&LIST_CSV_HEADER)).chain(rows).collect::<Vec<_>>().join("\n");
    finish(
        context,
        &StoreOutput {
            command: "restore",
            human: human::render_list(&data, context.home()?),
            csv: Some(table),
            data,
            errors: found.errors,
            warnings: context.warnings.clone(),
            host: context.host.clone(),
            generated_at: context.generated_at,
        },
    )
}

/// An entry as `--list` shows it: `planned`, not yet anywhere.
fn planned(entry: &QuarantineEntry) -> QuarantineEntry {
    QuarantineEntry { status: ItemStatus::Planned, restored_to: None, error: None, ..entry.clone() }
}

/// One `restore --list --csv` row.
fn list_row(entry: &QuarantineEntry) -> String {
    csv::row(&[
        entry.id.to_string(),
        entry.original_path.display().to_string(),
        entry.size_bytes.to_string(),
        "planned".to_owned(),
    ])
}

/// The sessions or entries the flags name.
fn wanted_of(args: &RestoreArgs, context: &StoreContext<'_>, root: &Path) -> Result<Wanted, BrozaError> {
    if args.all {
        return Ok(Wanted::Sessions(store::all_ids(context.ports.fs.as_ref(), root)?));
    }
    if let Some(session) = &args.session {
        return Ok(Wanted::Sessions(vec![parse_session_id(session)?]));
    }
    let entries: Vec<EntryId> = args.ids.iter().filter_map(|raw| raw.parse::<EntryId>().ok()).collect();
    if entries.len() == args.ids.len() {
        return Ok(Wanted::Entries(entries));
    }
    if !entries.is_empty() {
        return Err(BrozaError::Usage(
            "restore takes either session ids or item ids, not both at once".into(),
        ));
    }
    args.ids.iter().map(|raw| parse_session_id(raw)).collect::<Result<Vec<_>, _>>().map(Wanted::Sessions)
}

/// Restore whole sessions one after the other, folding the reports together.
fn restore_sessions(
    context: &StoreContext<'_>,
    root: &Path,
    mounts: &MountTable,
    ids: &[SessionId],
    to: Option<&Path>,
) -> Result<Reported<RestoreReport>, BrozaError> {
    let fs = context.ports.fs.as_ref();
    let mut folded = Reported::new(RestoreReport {
        operation: OperationKind::Restore,
        restored_bytes: 0,
        sessions: Vec::new(),
    });
    for id in ids {
        let wanted = session_destinations(fs, root, id, to)?;
        let targets = approve_restore_targets(&wanted, &restore_request(context, root, to)?, mounts, fs)?;
        let sources = session_write_token(context.ports, root, mounts, std::slice::from_ref(id))?;
        let one = restore_session(&sources, &targets, id, root, fs, to)?;
        folded = Reported::with(
            RestoreReport {
                operation: OperationKind::Restore,
                restored_bytes: folded.data.restored_bytes.saturating_add(one.data.restored_bytes),
                sessions: [folded.data.sessions, one.data.sessions].concat(),
            },
            [folded.errors, one.errors].concat(),
            [folded.warnings, one.warnings].concat(),
        );
    }
    Ok(folded)
}

/// The token over the destinations of `ids`.
fn target_token(
    context: &StoreContext<'_>,
    root: &Path,
    mounts: &MountTable,
    ids: &[EntryId],
    to: Option<&Path>,
) -> Result<broza::safety::guard::Approved<broza::safety::guard::RestoreWrite>, BrozaError> {
    let fs = context.ports.fs.as_ref();
    let wanted = entry_destinations(fs, root, ids, to)?;
    Ok(approve_restore_targets(&wanted, &restore_request(context, root, to)?, mounts, fs)?)
}

/// What the guard needs to know about this restore.
fn restore_request(
    context: &StoreContext<'_>,
    root: &Path,
    to: Option<&Path>,
) -> Result<RestoreRequest, BrozaError> {
    Ok(RestoreRequest {
        home: context.home()?.to_path_buf(),
        uid_temp_dirs: context.uid_temp_dirs.clone(),
        to: to.map(Path::to_path_buf),
        quarantine_root: Some(root.to_path_buf()),
    })
}
