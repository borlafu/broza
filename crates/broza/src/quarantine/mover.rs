//! Moving the items of an approved plan into the quarantine store.
//!
//! The order is the one ADR 0004 fixes, chosen so that a crash at any point
//! leaves a store a later run can still read:
//!
//! 1. create the session directory and write a manifest in state `in_progress`;
//! 2. for each item, in plan order: re-`lstat` and compare `(device, inode)`
//!    with the token, compare the device with the store's, measure a directory,
//!    `rename` ([`attempt`](crate::quarantine::attempt));
//! 3. rewrite the manifest after every item (temp file + rename);
//! 4. write the manifest one last time in state `complete`.
//!
//! Nothing is ever removed here. A failed item stays exactly where it was and is
//! recorded as `failed` or `skipped`, and the run continues
//! (`docs/cli-spec.md` §3.4).

use std::path::PathBuf;
use std::time::Duration;

use crate::BrozaError;
use crate::model::{CleanPlan, ItemStatus, QuarantineSession, SessionState};
use crate::ports::{Clock, FileOps};
use crate::quarantine::attempt::{Destination, attempt_move, moved_of, outcome_of, updated_entry};
use crate::quarantine::entries::{planned_entries, sequence_of, with_entry};
use crate::quarantine::manifest::{self, Manifest};
use crate::quarantine::{layout, ttl};
use crate::safety::guard::{Approved, ApprovedItem, Write};

/// Where a move puts its items, and under which limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRequest {
    /// Root of the quarantine store; the session directory is created inside it.
    pub root: PathBuf,
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
}

/// Move every approved item into a new session of the store.
///
/// `quarantined_bytes` counts what was actually moved, measured rather than
/// planned; `reclaimed_bytes` is left as the plan had it, because a quarantined
/// item still occupies the disk (`AGENTS.md` §2.7).
///
/// # Errors
///
/// Only for failures of the store itself: the root cannot be read, the session
/// directory cannot be created, the manifest cannot be written, or the token and
/// the plan it carries do not describe the same work. The failure of a single
/// *item* is recorded in the manifest and in the plan, and the move goes on.
pub fn quarantine_items(
    token: &Approved<Write>,
    request: &MoveRequest,
    fs: &dyn FileOps,
    clock: &dyn Clock,
) -> Result<MoveOutcome, BrozaError> {
    let plan = token.plan().clone();
    let context = Context::new(request, &plan, token.items(), fs)?;
    fs.create_dir_all(&context.dir)?;
    let session = new_session(&plan, &context, clock, request.ttl)?;
    let progress = Progress { manifest: Manifest::new(session), plan, moved_bytes: 0 };
    manifest::write(fs, &context.manifest, &progress.manifest)?;
    let moved =
        token.items().iter().enumerate().try_fold(progress, |progress, (position, item)| {
            move_one(progress, position, item, &context, fs)
        })?;
    finish(moved, &context, fs)
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
        request: &MoveRequest,
        plan: &CleanPlan,
        approved: &[ApprovedItem],
        fs: &dyn FileOps,
    ) -> Result<Self, BrozaError> {
        let dir = layout::session_dir(&request.root, plan.session_id());
        Ok(Self {
            manifest: layout::manifest_path(&dir),
            dir,
            root_device: fs.metadata(&request.root)?.device,
            max_size: request.max_size,
            plan_indices: plan_indices(plan, approved)?,
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
}

/// One item: attempt the move, record it, and persist the manifest.
fn move_one(
    progress: Progress,
    position: usize,
    item: &ApprovedItem,
    context: &Context,
    fs: &dyn FileOps,
) -> Result<Progress, BrozaError> {
    let destination = Destination {
        session_dir: &context.dir,
        sequence: sequence_of(position)?,
        root_device: context.root_device,
        max_size: context.max_size,
    };
    let attempt = attempt_move(item, &destination, progress.moved_bytes, fs);
    let Progress { manifest, plan, moved_bytes } = progress;
    let Manifest { manifest_version, session } = manifest;
    let entry = session
        .entries
        .get(position)
        .ok_or_else(|| desynchronised(&format!("the manifest has no entry {position}")))?;
    let session = with_entry(&session, position, &updated_entry(entry, &attempt));
    let index = *context
        .plan_indices
        .get(position)
        .ok_or_else(|| desynchronised(&format!("no plan item for approved item {position}")))?;
    let (status, error) = outcome_of(&attempt);
    let progress = Progress {
        manifest: Manifest { manifest_version, session },
        plan: plan.with_item_status(index, status, error)?,
        moved_bytes: moved_bytes.saturating_add(moved_of(&attempt)),
    };
    manifest::write(fs, &context.manifest, &progress.manifest)?;
    Ok(progress)
}

/// Close the session: mark it `complete` and fill in the plan's counters.
fn finish(progress: Progress, context: &Context, fs: &dyn FileOps) -> Result<MoveOutcome, BrozaError> {
    let Progress { manifest, plan, moved_bytes } = progress;
    let Manifest { manifest_version, session } = manifest;
    let manifest = Manifest {
        manifest_version,
        session: QuarantineSession { state: SessionState::Complete, ..session },
    };
    manifest::write(fs, &context.manifest, &manifest)?;
    let holds_items = manifest.session.entries.iter().any(|entry| entry.status == ItemStatus::Quarantined);
    let reclaimed_bytes = plan.reclaimed_bytes();
    let plan = plan
        .into_applied(holds_items.then(|| context.dir.clone()))?
        .with_bytes(moved_bytes, reclaimed_bytes)?;
    Ok(MoveOutcome { session: manifest.session, plan })
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
