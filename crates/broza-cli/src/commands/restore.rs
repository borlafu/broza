//! `broza restore` (`docs/cli-spec.md` §3.5 and §4.6).
//!
//! Items go back to where they came from, or under `--to`. Every identifier is
//! resolved against the store *before* anything moves (an unknown one is exit
//! `4` with nothing written); then two tokens authorise the whole run: one over
//! exactly the stored items that leave the store, one over the destinations the
//! guard checks against the allowlist, `--to` included. `--list` reads the
//! store and writes nothing.

use std::path::{Path, PathBuf};

use broza::BrozaError;
use broza::model::{
    EntryId, ItemStatus, OperationKind, QuarantineEntry, RestoreReport, RestoreSession, Warning,
};
use broza::quarantine::store::StoredSession;
use broza::quarantine::{Wanted, destinations, group_by_session, layout, restore_wanted, store};
use broza::safety::guard::{RestoreRequest, approve_quarantine_write, approve_restore_targets};

use crate::args::RestoreArgs;
use crate::commands::Outcome;
use crate::commands::store::{StoreContext, StoreOutput, finish, mounts, parse_session_id, root_of};
use crate::output::csv;
use crate::output::human::restore as human;

/// Header of `restore --list --csv` (`docs/cli-spec.md` §3.5).
const LIST_CSV_HEADER: [&str; 4] = ["id", "original_path", "size_bytes", "status"];

/// What the identifiers resolved to, with the store's own complaints.
struct Resolved {
    /// The sessions (and, for item ids, which of their entries) to restore.
    wanted: Vec<Wanted>,
    /// Sessions the store could not read (`--all` only): `errors[]`, exit `5`.
    errors: Vec<Warning>,
}

/// Run `restore`.
///
/// # Errors
///
/// `HOME` unset or an identifier of neither form (exit `2`); an unknown session
/// or item (exit `4`, before anything is written); whatever the guard refuses
/// about a destination. A session that cannot be read or an item that cannot
/// go back is an `errors[]` entry and exit `5`, never an abort.
pub fn run(args: &RestoreArgs, context: &StoreContext<'_>) -> Result<Outcome, BrozaError> {
    let home = context.home()?;
    let root = root_of(context.config, home);
    if args.list {
        return list(context, &root);
    }
    let fs = context.ports.fs.as_ref();
    let to = args.to.as_deref();
    let resolved = resolve(args, fs, &root)?;
    if resolved.wanted.is_empty() {
        // `--all` over an empty (or wholly unreadable) store: no token is needed
        // for a run that writes nothing, and the store may not even exist yet.
        let data =
            RestoreReport { operation: OperationKind::Restore, restored_bytes: 0, sessions: Vec::new() };
        return report(context, data, resolved.errors, Vec::new(), home);
    }
    let (mounts, mount_warnings) = mounts(context.ports)?;
    let targets = approve_restore_targets(
        &destinations(fs, &root, &resolved.wanted, to)?,
        &restore_request(context, &root, to)?,
        &mounts,
        fs,
    )?;
    let sources = approve_quarantine_write(&source_paths(fs, &root, &resolved.wanted)?, &root, &mounts, fs)?;
    let reported = restore_wanted(&sources, &targets, &resolved.wanted, &root, fs, to)?;
    let errors = [resolved.errors, reported.errors].concat();
    report(context, reported.data, errors, [mount_warnings, reported.warnings].concat(), home)
}

/// The three renderings of a restore, with the run's diagnostics.
fn report(
    context: &StoreContext<'_>,
    data: RestoreReport,
    errors: Vec<Warning>,
    warnings: Vec<Warning>,
    home: &Path,
) -> Result<Outcome, BrozaError> {
    finish(
        context.format,
        &StoreOutput {
            command: "restore",
            human: human::render_restore(&data, &errors, home),
            csv: None,
            data,
            errors,
            warnings: [context.warnings.clone(), warnings].concat(),
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
        context.format,
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

/// Every identifier the flags name, checked against the store before any write.
fn resolve(args: &RestoreArgs, fs: &dyn broza::ports::FileOps, root: &Path) -> Result<Resolved, BrozaError> {
    if args.all {
        let found = store::read_all(fs, root)?;
        let wanted = found.sessions.iter().map(|stored| Wanted::whole(&stored.id)).collect();
        return Ok(Resolved { wanted, errors: found.errors });
    }
    let wanted = if let Some(session) = &args.session {
        vec![Wanted::whole(&parse_session_id(session)?)]
    } else {
        wanted_of_ids(&args.ids)?
    };
    for one in &wanted {
        check_known(fs, root, one)?;
    }
    Ok(Resolved { wanted, errors: Vec::new() })
}

/// Positional ids as sessions or as items; one kind per invocation.
fn wanted_of_ids(ids: &[String]) -> Result<Vec<Wanted>, BrozaError> {
    let entries: Vec<EntryId> = ids.iter().filter_map(|raw| raw.parse::<EntryId>().ok()).collect();
    if entries.len() == ids.len() {
        return group_by_session(&entries);
    }
    if !entries.is_empty() {
        return Err(BrozaError::Usage(
            "restore takes either session ids or item ids, not both at once".into(),
        ));
    }
    ids.iter().map(|raw| parse_session_id(raw).map(|id| Wanted::whole(&id))).collect()
}

/// The session must exist, and every item asked for must still be in it.
fn check_known(fs: &dyn broza::ports::FileOps, root: &Path, wanted: &Wanted) -> Result<(), BrozaError> {
    let found = store::read_one(fs, root, &wanted.session)?;
    let Some(entries) = &wanted.entries else { return Ok(()) };
    for id in entries {
        let present =
            found.session().entries.iter().any(|entry| &entry.id == id && entry.stored_path.is_some());
        if !present {
            return Err(BrozaError::TargetNotFound(format!("item `{id}` is not in quarantine")));
        }
    }
    Ok(())
}

/// The stored items that will leave the store, plus their session directories.
fn source_paths(
    fs: &dyn broza::ports::FileOps,
    root: &Path,
    wanted: &[Wanted],
) -> Result<Vec<PathBuf>, BrozaError> {
    let mut paths = Vec::new();
    for one in wanted {
        let found: StoredSession = store::read_one(fs, root, &one.session)?;
        paths.extend(
            found
                .session()
                .entries
                .iter()
                .filter(|entry| one.covers(entry))
                .filter_map(|entry| entry.stored_path.clone()),
        );
        paths.push(layout::session_dir(root, &one.session));
    }
    Ok(paths)
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
