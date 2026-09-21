//! What `restore`, `quarantine` and the expiry step of `clean` share: where the
//! store is, the mount table the guard needs, the tokens that authorise writing
//! inside the store, and the two prompts (`y/N`, typed `PURGE`).
//!
//! Every removal inside the store goes through an
//! [`Approved<QuarantineWrite>`](broza::safety::guard::QuarantineWrite) issued
//! here over the session directories; `restore` builds its own over exactly the
//! stored items it puts back. Nothing in the CLI builds one any other way.

use std::path::{Path, PathBuf};

use broza::BrozaError;
use broza::model::{Risk, SessionId, Warning};
use broza::ports::{Answer, ConfirmationRequest, Ports};
use broza::quarantine::{layout, store};
use broza::safety::PURGE_LITERAL;
use broza::safety::guard::{Approved, QuarantineWrite, approve_quarantine_write};
use broza::scan::MountTable;

use crate::commands::mount::mount_table;

/// The mount table, from a fresh enumeration, with the warnings it raised.
///
/// # Errors
///
/// Whatever enumerating the disks or reading the mount table reports.
pub fn mounts(ports: &Ports) -> Result<(MountTable, Vec<Warning>), BrozaError> {
    let enumeration = ports.disks.enumerate()?;
    let mount = mount_table(ports, &enumeration.disks)?;
    Ok((mount.table, [enumeration.warnings, mount.warnings].concat()))
}

/// A token covering only the session directories of `ids`, for removing them.
///
/// `expire` and `purge` re-check the session directory before `remove_tree`
/// and never the items inside, so approving each stored item would cost one
/// `lstat` walk per item for nothing — and would let a stored file removed out
/// of band fail the whole operation.
///
/// # Errors
///
/// Whatever the guard refuses about the store or a session directory.
pub fn session_dirs_token(
    ports: &Ports,
    root: &Path,
    mounts: &MountTable,
    ids: &[SessionId],
) -> Result<Approved<QuarantineWrite>, BrozaError> {
    let paths: Vec<PathBuf> = ids.iter().map(|id| layout::session_dir(root, id)).collect();
    Ok(approve_quarantine_write(&paths, root, mounts, ports.fs.as_ref())?)
}

/// The bytes each of `ids` holds, as the store reports them.
///
/// # Errors
///
/// [`BrozaError::TargetNotFound`] for an unknown identifier.
pub fn session_sizes(
    ports: &Ports,
    root: &Path,
    ids: &[SessionId],
) -> Result<Vec<(SessionId, u64)>, BrozaError> {
    ids.iter()
        .map(|id| {
            store::read_one(ports.fs.as_ref(), root, id)
                .map(|found| (id.clone(), found.session().total_bytes))
        })
        .collect()
}

/// A `y/N` confirmation for removing `sessions`, unless `yes` covers it.
///
/// # Errors
///
/// [`BrozaError::ConfirmationRequired`] (exit `7`) without a terminal,
/// [`BrozaError::AbortedByUser`] (exit `6`) on `n`.
pub fn confirm_removal(
    ports: &Ports,
    sessions: &[(SessionId, u64)],
    yes: bool,
    what: &str,
) -> Result<(), BrozaError> {
    if yes {
        return Ok(());
    }
    answer_to_result(ports.prompter.confirm(&removal_request(sessions, what)))
}

/// The typed-`PURGE` confirmation for deleting `sessions` regardless of age.
///
/// `--yes` never reaches here: the CLI refuses it beside `purge` (exit `2`).
///
/// # Errors
///
/// See [`confirm_removal`].
pub fn confirm_purge(ports: &Ports, sessions: &[(SessionId, u64)]) -> Result<(), BrozaError> {
    answer_to_result(ports.prompter.confirm_literal(&removal_request(sessions, "purge"), PURGE_LITERAL))
}

fn answer_to_result(answer: Answer) -> Result<(), BrozaError> {
    match answer {
        Answer::Yes => Ok(()),
        Answer::No => Err(BrozaError::AbortedByUser),
        Answer::NoTty => Err(BrozaError::ConfirmationRequired),
    }
}

/// The prompt for removing sessions: green-level words, irreversible flag set.
/// Shared with the expiry step of `clean` so the two prompts cannot drift.
pub(crate) fn removal_request(sessions: &[(SessionId, u64)], what: &str) -> ConfirmationRequest {
    ConfirmationRequest {
        max_risk: Risk::Green,
        item_count: sessions.len(),
        total_bytes: sessions.iter().map(|(_, bytes)| *bytes).fold(0, u64::saturating_add),
        irreversible: true,
        preview: sessions.iter().map(|(id, _)| format!("{what} quarantine session {id}")).collect(),
    }
}

/// `raw` as a session identifier, or the usage error naming it.
///
/// # Errors
///
/// [`BrozaError::Usage`] when `raw` is not `cln_YYYYMMDDHHMMSS_xxxx`.
pub fn parse_session_id(raw: &str) -> Result<SessionId, BrozaError> {
    raw.parse::<SessionId>().map_err(|error| {
        BrozaError::Usage(format!("`{raw}` is not a session id (cln_YYYYMMDDHHMMSS_xxxx): {error}"))
    })
}

/// The quarantine store for `home`, from the configuration.
pub fn root_of(config: &broza::config::Config, home: &Path) -> PathBuf {
    config.quarantine_dir(home)
}

/// Everything the store commands need beyond their own arguments.
pub struct StoreContext<'a> {
    /// The wired adapters.
    pub ports: &'a Ports,
    /// The effective configuration (`quarantine-ttl`, `quarantine-path`).
    pub config: &'a broza::config::Config,
    /// `host` block of the envelope.
    pub host: broza::model::Host,
    /// `generated_at` of the envelope.
    pub generated_at: jiff::Timestamp,
    /// Warnings raised before the command ran.
    pub warnings: Vec<Warning>,
    /// Format the caller asked for.
    pub format: crate::output::OutputFormat,
    /// The user's home, for the store location and the restore allowlist.
    pub home: Option<PathBuf>,
    /// Per-uid temporary directories the guard may allow restores into.
    pub uid_temp_dirs: Vec<PathBuf>,
}

impl StoreContext<'_> {
    /// The home, or the usage error a command without one reports.
    ///
    /// # Errors
    ///
    /// [`BrozaError::Usage`] when `HOME` is unset.
    pub fn home(&self) -> Result<&Path, BrozaError> {
        self.home.as_deref().ok_or_else(|| {
            BrozaError::Usage("HOME is not set: the quarantine store lives under the home directory".into())
        })
    }

    /// The retention period from the configuration.
    pub fn ttl(&self) -> std::time::Duration {
        self.config.quarantine_ttl.to_duration()
    }
}

/// A store command's result in the three renderings, ready for the sink.
#[derive(Debug, Clone)]
pub struct StoreOutput<T> {
    /// Command name in the envelope.
    pub command: &'static str,
    /// The payload.
    pub data: T,
    /// The human text.
    pub human: String,
    /// The CSV table, for the commands that have one.
    pub csv: Option<String>,
    /// `errors[]` of the envelope.
    pub errors: Vec<Warning>,
    /// `warnings[]` of the envelope.
    pub warnings: Vec<Warning>,
    /// `host` block of the envelope.
    pub host: broza::model::Host,
    /// `generated_at` of the envelope.
    pub generated_at: jiff::Timestamp,
}

impl<T: serde::Serialize + Clone> crate::output::Renderer for StoreOutput<T> {
    fn to_human(&self) -> String {
        self.human.clone()
    }

    fn to_csv(&self) -> Result<String, BrozaError> {
        self.csv.clone().ok_or_else(|| BrozaError::Usage("this command has no CSV output".to_owned()))
    }

    fn to_json(&self) -> Result<String, BrozaError> {
        let envelope = broza::model::Envelope::new(
            self.command,
            self.host.clone(),
            self.generated_at,
            self.data.clone(),
        );
        let envelope = self.warnings.iter().cloned().fold(envelope, broza::model::Envelope::with_warning);
        let envelope = self.errors.iter().cloned().fold(envelope, broza::model::Envelope::with_error);
        crate::output::envelope_to_json(&envelope)
    }
}

/// Fold a store command's parts into the [`Outcome`](crate::commands::Outcome):
/// exit `5` when anything is in `errors[]`, `0` otherwise.
///
/// # Errors
///
/// When the payload cannot be serialised.
pub fn finish<T: serde::Serialize + Clone>(
    format: crate::output::OutputFormat,
    output: &StoreOutput<T>,
) -> Result<crate::commands::Outcome, BrozaError> {
    use crate::output::Renderer as _;
    let code = if output.errors.is_empty() { broza::ExitCode::Ok } else { broza::ExitCode::PartialFailure };
    let warnings = output.warnings.clone();
    Ok(crate::commands::Outcome::ok(output.render(format)?).with_warnings(warnings).with_code(code))
}
