//! Moving the items of an approved plan into the quarantine store.
//!
//! The order is the one ADR 0004 fixes, chosen so that a crash at any point
//! leaves a store a later run can still read:
//!
//! 1. claim the session directory *exclusively*, so two runs that derive the
//!    same identifier cannot share one session, and write a manifest in state
//!    `in_progress`;
//! 2. for each item, in plan order: re-`lstat` and compare `(device, inode)`
//!    with the token, compare the device with the store's, measure a directory
//!    ([`precheck`]);
//! 3. write the entry as `moving`, with the path it is going to, **before** the
//!    rename, and rewrite it as `quarantined` after — so an interrupted move
//!    leaves a trail either way and [`mod@crate::quarantine::reconcile`]
//!    can settle it;
//! 4. write the manifest one last time in state `complete`.
//!
//! Nothing is ever removed here. A failed item stays exactly where it was and is
//! recorded as `failed` or `skipped`, and the run continues
//! (`docs/cli-spec.md` §3.4).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::BrozaError;
use crate::model::{
    CleanPlan, CleanPlanRepr, ItemStatus, QuarantineSession, SessionId, SessionState, Warning,
};
use crate::ports::{Clock, FileOps, already_exists};
use crate::quarantine::attempt::{
    Attempt, Destination, in_flight, move_into, moved_of, outcome_of, precheck, updated_entry, warning_of,
};
use crate::quarantine::entries::{planned_entries, sequence_of, with_entry};
use crate::quarantine::lock;
use crate::quarantine::manifest::{self, Manifest};
use crate::quarantine::{layout, ttl};
use crate::safety::guard::{Approved, ApprovedItem, Write};

/// How many session identifiers the mover tries before giving up.
const MAX_SESSION_ATTEMPTS: u32 = 16;

/// Under which limits a move happens.
///
/// The store root is **not** here: it comes from the token, which carries the
/// one spelling of it the guard actually validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRequest {
    /// Retention period, from which `expires_at` is derived.
    pub ttl: Duration,
    /// `--max-size` cap in bytes, applied to the measured running total.
    pub max_size: Option<u64>,
}

/// What a move left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveOutcome {
    /// The session exactly as the manifest on disk now describes it.
    pub session: QuarantineSession,
    /// The plan, with each item's outcome and the byte counters filled in.
    pub plan: CleanPlan,
    /// What the user should know about how the items got there.
    pub warnings: Vec<Warning>,
}

/// Move every approved item into a new session of the store.
///
/// `quarantined_bytes` counts what was actually moved; `reclaimed_bytes` is left
/// as the plan had it, because a quarantined item still occupies the disk
/// (`AGENTS.md` §2.7).
///
/// # Errors
///
/// [`BrozaError::Other`] when the token carries no quarantine root (the guard
/// never validated one, so there is nowhere approved to write) or when the token
/// and the plan do not describe the same work, plus whatever claiming the
/// session directory or writing the manifest reports. The failure of a single
/// *item* is recorded in the manifest and in the plan, and the move goes on.
pub fn quarantine_items(
    token: &Approved<Write>,
    request: &MoveRequest,
    fs: &dyn FileOps,
    clock: &dyn Clock,
) -> Result<MoveOutcome, BrozaError> {
    let root = token.quarantine_root().ok_or_else(|| {
        desynchronised("the approval carries no quarantine store, so there is nowhere to move to")
    })?;
    fs.create_dir_all(root)?;
    let (plan, dir) = claim_session(root, token.plan(), fs)?;
    // Held until the move is over: nothing may expire, purge or restore a
    // session while its items are still arriving.
    let held = match lock::take(fs, &dir) {
        lock::Taken::Held(held) => held,
        lock::Taken::Unavailable(error) => return Err(error),
        lock::Taken::Busy => {
            return Err(desynchronised(&format!(
                "session `{}` is already being written by another Broza",
                plan.session_id()
            )));
        }
    };
    let context = Context::new(&plan, dir, token.items(), request, fs)?;
    let session = new_session(&plan, &context, clock, request.ttl)?;
    let progress = Progress::new(Manifest::new(session), plan);
    manifest::write(fs, &context.manifest, &progress.manifest)?;
    let moved =
        token.items().iter().enumerate().try_fold(progress, |progress, (position, item)| {
            move_one(progress, position, item, &context, fs)
        })?;
    let outcome = finish(moved, &context, fs);
    drop(held);
    outcome
}

/// Take a session directory nobody else has, and the plan that names it.
///
/// `create_dir_exclusive` is the whole mechanism: the first run to create the
/// directory owns the identifier, and a second run — same second, same clock,
/// same derived name — is told the name is taken and moves to the next one
/// rather than writing its items into the other run's session.
fn claim_session(
    root: &Path,
    plan: &CleanPlan,
    fs: &dyn FileOps,
) -> Result<(CleanPlan, PathBuf), BrozaError> {
    let mut id = plan.session_id().clone();
    for _ in 0..MAX_SESSION_ATTEMPTS {
        let dir = layout::session_dir(root, &id);
        match fs.create_dir_exclusive(&dir) {
            Ok(()) => return Ok((with_session_id(plan, id)?, dir)),
            Err(error) if already_exists(&error) => id = layout::next_session_id(&id)?,
            Err(error) => return Err(error),
        }
    }
    Err(desynchronised(&format!(
        "no free session identifier near `{}` after {MAX_SESSION_ATTEMPTS} tries",
        plan.session_id()
    )))
}

/// The same plan under another session identifier.
fn with_session_id(plan: &CleanPlan, session_id: SessionId) -> Result<CleanPlan, BrozaError> {
    if *plan.session_id() == session_id {
        return Ok(plan.clone());
    }
    CleanPlan::new(CleanPlanRepr {
        dry_run: plan.is_dry_run(),
        session_id,
        planned_bytes: plan.planned_bytes(),
        quarantined_bytes: plan.quarantined_bytes(),
        reclaimed_bytes: plan.reclaimed_bytes(),
        quarantine_path: plan.quarantine_path().cloned(),
        expired_sessions: plan.expired_sessions().to_vec(),
        items: plan.items().to_vec(),
    })
}

/// Everything the loop needs that does not change between items.
struct Context {
    /// Directory of the session.
    dir: PathBuf,
    /// Manifest of the session.
    manifest: PathBuf,
    /// Device the store sits on.
    root_device: u64,
    /// `--max-size` cap.
    max_size: Option<u64>,
    /// Index in the plan of each approved item, in approval order.
    plan_indices: Vec<usize>,
}

impl Context {
    fn new(
        plan: &CleanPlan,
        dir: PathBuf,
        approved: &[ApprovedItem],
        request: &MoveRequest,
        fs: &dyn FileOps,
    ) -> Result<Self, BrozaError> {
        Ok(Self {
            manifest: layout::manifest_path(&dir),
            root_device: fs.metadata(&dir)?.device,
            dir,
            max_size: request.max_size,
            plan_indices: plan_indices(plan, approved)?,
        })
    }

    /// Where the item at `position` is going.
    fn destination(&self, position: usize, planned_bytes: u64) -> Result<Destination<'_>, BrozaError> {
        Ok(Destination {
            session_dir: &self.dir,
            sequence: sequence_of(position)?,
            root_device: self.root_device,
            max_size: self.max_size,
            planned_bytes,
        })
    }
}

/// The manifest, the plan and the running total, carried through the loop.
struct Progress {
    /// The manifest as it was last written.
    manifest: Manifest,
    /// The plan with the outcomes recorded so far.
    plan: CleanPlan,
    /// Bytes moved so far, against which `--max-size` is checked.
    moved_bytes: u64,
    /// What the user should know about how the items got there.
    warnings: Vec<Warning>,
}

impl Progress {
    /// A run that has not moved anything yet.
    fn new(manifest: Manifest, plan: CleanPlan) -> Self {
        Self { manifest, plan, moved_bytes: 0, warnings: Vec::new() }
    }
}

/// One item: check it, announce the move, do it, and record what happened.
fn move_one(
    progress: Progress,
    position: usize,
    item: &ApprovedItem,
    context: &Context,
    fs: &dyn FileOps,
) -> Result<Progress, BrozaError> {
    let entry = entry_at(&progress.manifest, position)?;
    let destination = context.destination(position, entry.size_bytes)?;
    let size_bytes = match precheck(item, &destination, progress.moved_bytes, fs) {
        Ok(size_bytes) => size_bytes,
        Err(refused) => return record(progress, position, &refused, context, fs),
    };
    let stored = destination.stored_path(item.path());
    let announced = announce(progress, position, &in_flight(&entry, &stored, size_bytes), context, fs)?;
    let attempt = move_into(item, &destination, size_bytes, fs);
    record(announced, position, &attempt, context, fs)
}

/// Persist the entry that says the item may already be in either place.
fn announce(
    progress: Progress,
    position: usize,
    flying: &crate::model::QuarantineEntry,
    context: &Context,
    fs: &dyn FileOps,
) -> Result<Progress, BrozaError> {
    let Progress { manifest, plan, moved_bytes, warnings } = progress;
    let session = with_entry(&manifest.session, position, flying);
    let manifest = manifest.with_session(session);
    manifest::write(fs, &context.manifest, &manifest)?;
    Ok(Progress { manifest, plan, moved_bytes, warnings })
}

/// Fold the outcome into the manifest, the plan and the running total.
fn record(
    progress: Progress,
    position: usize,
    attempt: &Attempt,
    context: &Context,
    fs: &dyn FileOps,
) -> Result<Progress, BrozaError> {
    let Progress { manifest, plan, moved_bytes, warnings } = progress;
    let entry = entry_at(&manifest, position)?;
    let session = with_entry(&manifest.session, position, &updated_entry(&entry, attempt));
    let index = *context
        .plan_indices
        .get(position)
        .ok_or_else(|| desynchronised(&format!("no plan item for approved item {position}")))?;
    let (status, error) = outcome_of(attempt);
    let progress = Progress {
        manifest: manifest.with_session(session),
        plan: plan.with_item_status(index, status, error)?,
        moved_bytes: moved_bytes.saturating_add(moved_of(attempt)),
        warnings: [warnings, warning_of(attempt).into_iter().collect()].concat(),
    };
    manifest::write(fs, &context.manifest, &progress.manifest)?;
    Ok(progress)
}

/// The entry the manifest holds for the item at `position`.
fn entry_at(manifest: &Manifest, position: usize) -> Result<crate::model::QuarantineEntry, BrozaError> {
    manifest
        .session
        .entries
        .get(position)
        .cloned()
        .ok_or_else(|| desynchronised(&format!("the manifest has no entry {position}")))
}

/// Close the session: mark it `complete` and fill in the plan's counters.
fn finish(progress: Progress, context: &Context, fs: &dyn FileOps) -> Result<MoveOutcome, BrozaError> {
    let Progress { manifest, plan, moved_bytes, warnings } = progress;
    let Manifest { manifest_version, session } = manifest;
    let manifest = Manifest {
        manifest_version,
        session: QuarantineSession { state: SessionState::Complete, ..session },
    };
    manifest::write(fs, &context.manifest, &manifest)?;
    let holds_items = manifest.session.entries.iter().any(|entry| entry.stored_path.is_some());
    let reclaimed_bytes = plan.reclaimed_bytes();
    let plan = plan
        .into_applied(holds_items.then(|| context.dir.clone()))?
        .with_bytes(moved_bytes, reclaimed_bytes)?;
    Ok(MoveOutcome { session: manifest.session, plan, warnings })
}

/// A fresh session: one planned entry per approved item, nothing moved yet.
fn new_session(
    plan: &CleanPlan,
    context: &Context,
    clock: &dyn Clock,
    retention: Duration,
) -> Result<QuarantineSession, BrozaError> {
    let created_at = clock.now();
    let entries = planned_entries(plan, &context.plan_indices)?;
    Ok(QuarantineSession {
        id: plan.session_id().clone(),
        created_at,
        expires_at: ttl::expires_at(created_at, retention),
        total_bytes: 0,
        item_count: u64::try_from(entries.len()).unwrap_or(u64::MAX),
        state: SessionState::InProgress,
        entries,
    })
}

/// Where each approved item sits in the plan.
///
/// The guard hands over evidence only for the paths that survived its checks,
/// in plan order, so the two lists are matched by walking them together. A path
/// that cannot be matched is a bug in the caller, not an item failure: pairing
/// one path's evidence with another path is exactly what the token prevents.
fn plan_indices(plan: &CleanPlan, approved: &[ApprovedItem]) -> Result<Vec<usize>, BrozaError> {
    let items = plan.items();
    let mut cursor = 0_usize;
    let mut indices = Vec::with_capacity(approved.len());
    for item in approved {
        let offset = items
            .get(cursor..)
            .unwrap_or_default()
            .iter()
            .position(|planned| planned.path == item.path() && planned.status == ItemStatus::Planned)
            .ok_or_else(|| desynchronised(&format!("no planned item for `{}`", item.path().display())))?;
        indices.push(cursor + offset);
        cursor += offset + 1;
    }
    Ok(indices)
}

/// A plan and a token that do not describe the same work.
fn desynchronised(reason: &str) -> BrozaError {
    BrozaError::Other(format!("quarantine move: {reason}"))
}
