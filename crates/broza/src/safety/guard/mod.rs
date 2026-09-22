//! The guard: the only place in Broza that can produce an [`Approved`] token.
//!
//! `docs/adr/0003-approved-token-safety-kernel.md`. Every mutating function takes
//! `&Approved<_>`; `grep -rn "Approved<" crates/` therefore enumerates every write
//! path in the code base.
//!
//! The flow of one `clean --apply` is:
//!
//! ```text
//! approve(outcome, findings, request, mounts, fs)  checks of docs/cli-spec.md 3.4
//!   -> Verdict::Nothing                             nothing left to write, exit 0
//!   -> Verdict::DryRun                           nothing may be written
//!   -> Verdict::NeedsConfirmation(p)             p.confirm(prompter)
//!        -> Approved<Write>                      the executor's ticket
//! ```
//!
//! The token carries the plan **and** an [`ApprovedItem`] per path with the
//! `(device, inode)` observed during the checks. Those two numbers are how the
//! executor closes the time-of-check/time-of-use gap: see [`ApprovedItem`].

mod checks;
mod item;
mod narrow;
mod rebuild;
mod restore;
mod token;
mod verdict;

pub use checks::approve;
pub use narrow::{approve_quarantine_write, snapshot_deletions};
pub use restore::{RestoreRequest, approve_restore_targets};
pub use token::{
    Approved, ApprovedItem, ApprovedPlan, QuarantineWrite, RestoreWrite, SnapshotDelete, Write, WriteKind,
};

/// Shared with `clean::planner` so the plan and the guard agree on what
/// `--purge` means; see [`item::expected_action`].
pub(crate) use item::expected_action;

use std::path::PathBuf;

use crate::model::{CleanPlan, Risk};
use crate::ports::{Answer, ConfirmationRequest, Prompter};
use crate::safety::exclusions::Exclusions;
use crate::safety::guard::token::issue;
use crate::safety::policy::{ConfirmationMode, PolicyInput};
use crate::safety::rejection::{GuardRejection, PolicyError};
use crate::safety::roots::AllowedRoots;

/// Types only nameable inside this module; they are what makes the token unforgeable.
mod seal {
    /// Private field of [`super::Approved`]: no other module can write it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Seal;

    /// Supertrait that stops foreign implementations of [`super::WriteKind`].
    pub trait Sealed {}
}

/// Everything the guard needs to know about the invocation.
///
/// The risk level is *not* here: the guard derives it from the findings the plan
/// refers to, so a caller cannot understate it.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRequest {
    /// `--apply`; without it the verdict is always a dry run.
    pub apply: bool,
    /// `--purge`: irreversible deletion instead of quarantine.
    pub purge: bool,
    /// `--yes`.
    pub yes: bool,
    /// An interactive terminal is available.
    pub tty: bool,
    /// The `CI` environment variable is set.
    pub ci: bool,
    /// `--max-size` cap in bytes.
    pub max_size: Option<u64>,
    /// Exclusions from the configuration and the command line.
    pub exclusions: Exclusions,
    /// Root of the quarantine store, when one is configured. Nothing inside it is
    /// ever part of a clean plan: that is where the previous plan put its items.
    pub quarantine_root: Option<PathBuf>,
    /// The user's home directory (`/Users/<name>`).
    pub home: PathBuf,
    /// Per-uid temporary directories (`/private/var/folders/<xx>/<hash>`).
    pub uid_temp_dirs: Vec<PathBuf>,
}

impl WriteRequest {
    /// A request that writes nothing: every flag off, no cap, no exclusion.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self {
            apply: false,
            purge: false,
            yes: false,
            tty: false,
            ci: false,
            max_size: None,
            exclusions: Exclusions::none(),
            quarantine_root: None,
            home: home.into(),
            uid_temp_dirs: Vec::new(),
        }
    }

    fn policy_input(&self, max_risk: Option<Risk>, irreversible: bool) -> PolicyInput {
        PolicyInput {
            apply: self.apply,
            max_risk,
            purge: self.purge,
            irreversible,
            yes: self.yes,
            tty: self.tty,
            ci: self.ci,
        }
    }

    fn allowed_roots(&self) -> Result<AllowedRoots, GuardRejection> {
        AllowedRoots::new(&self.home, &self.uid_temp_dirs)
    }
}

/// What the guard decided.
#[derive(Debug)]
pub enum Verdict {
    /// Nothing is left to write: the selection was empty, or every path it named
    /// has since vanished (exit `0`). The plan is carried along so the report can
    /// still show the skipped items.
    Nothing(CleanPlan),
    /// No `--apply`: the plan is reported and nothing is written.
    DryRun(CleanPlan),
    /// Every safety check passed; the confirmation still has to happen.
    NeedsConfirmation(PendingApproval),
}

/// A plan that passed every check and is waiting for the user's answer.
///
/// The only way out is [`PendingApproval::confirm`], which picks the prompt the
/// mode requires. There is deliberately no way to hand in an answer directly: a
/// plain "yes" must never satisfy a `--purge`, which demands the typed word.
#[derive(Debug)]
pub struct PendingApproval {
    payload: ApprovedPlan,
    mode: ConfirmationMode,
    request: ConfirmationRequest,
    /// Never read; see [`Approved`].
    #[allow(dead_code)]
    seal: seal::Seal,
}

impl PendingApproval {
    /// The plan that will be executed once confirmed.
    pub fn plan(&self) -> &CleanPlan {
        &self.payload.plan
    }

    /// The paths that passed the checks, with their identity at that moment.
    pub fn items(&self) -> &[ApprovedItem] {
        &self.payload.items
    }

    /// How the user must confirm.
    pub fn mode(&self) -> ConfirmationMode {
        self.mode
    }

    /// What to show the user.
    pub fn request(&self) -> &ConfirmationRequest {
        &self.request
    }

    /// Runs the prompt this plan requires and turns the answer into a token.
    ///
    /// `Yes` produces the token, `No` aborts (exit `6`) and `NoTty` means the
    /// confirmation could not be obtained (exit `7`). For
    /// [`ConfirmationMode::TypedLiteral`] the answer must come from
    /// [`Prompter::confirm_literal`]: that is the only way the typed `PURGE`
    /// reaches the guard.
    pub fn confirm(self, prompter: &dyn Prompter) -> Result<Approved<Write>, GuardRejection> {
        let answer = match self.mode {
            ConfirmationMode::None => Answer::Yes,
            ConfirmationMode::SimpleYesNo | ConfirmationMode::DetailedExplicit => {
                prompter.confirm(&self.request)
            }
            ConfirmationMode::TypedLiteral(word) => prompter.confirm_literal(&self.request, word),
            ConfirmationMode::Rejected(reason) => return Err(PolicyError::Rejected(reason).into()),
            ConfirmationMode::RequiredButNoTty => {
                return Err(PolicyError::ConfirmationRequired.into());
            }
        };
        self.seal(answer)
    }

    /// Private: an answer only ever comes from the prompt [`Self::confirm`] ran.
    fn seal(self, answer: Answer) -> Result<Approved<Write>, GuardRejection> {
        match answer {
            Answer::Yes => Ok(issue::<Write>(self.payload)),
            Answer::No => Err(PolicyError::AbortedByUser.into()),
            Answer::NoTty => Err(PolicyError::ConfirmationRequired.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::token::evidence_for;
    use super::{Approved, ApprovedPlan, PendingApproval, QuarantineWrite, Write, WriteRequest, issue, seal};
    use crate::model::{CleanPlan, Risk, SessionId};
    use crate::ports::{Answer, ConfirmationRequest, Prompter};
    use crate::safety::policy::{ConfirmationMode, PURGE_LITERAL, RejectReason};
    use crate::safety::rejection::{GuardRejection, PolicyError};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn session() -> SessionId {
        "cln_20260921103608_a1b2".parse().unwrap_or_else(|error| panic!("{error}"))
    }

    fn payload() -> ApprovedPlan {
        ApprovedPlan::new(
            CleanPlan::dry_run(session(), Vec::new()).unwrap_or_else(|error| panic!("{error}")),
            vec![evidence_for("/Users/dana/x", 2, 7)],
        )
    }

    fn pending(mode: ConfirmationMode) -> PendingApproval {
        PendingApproval {
            payload: payload(),
            mode,
            request: ConfirmationRequest {
                max_risk: Risk::Green,
                item_count: 0,
                total_bytes: 0,
                irreversible: false,
                preview: Vec::new(),
            },
            seal: seal::Seal,
        }
    }

    /// Answers each kind of prompt separately and counts both.
    struct ScriptedPrompter {
        on_confirm: Answer,
        on_literal: Answer,
        confirms: AtomicUsize,
        literals: AtomicUsize,
    }

    impl ScriptedPrompter {
        fn new(on_confirm: Answer, on_literal: Answer) -> Self {
            Self { on_confirm, on_literal, confirms: AtomicUsize::new(0), literals: AtomicUsize::new(0) }
        }

        fn always(answer: Answer) -> Self {
            Self::new(answer, answer)
        }
    }

    impl Prompter for ScriptedPrompter {
        fn confirm(&self, _request: &ConfirmationRequest) -> Answer {
            self.confirms.fetch_add(1, Ordering::Relaxed);
            self.on_confirm
        }

        fn confirm_literal(&self, _request: &ConfirmationRequest, expected: &str) -> Answer {
            assert_eq!(expected, PURGE_LITERAL);
            self.literals.fetch_add(1, Ordering::Relaxed);
            self.on_literal
        }
    }

    #[test]
    fn yes_produces_a_token_and_no_aborts() {
        let yes = pending(ConfirmationMode::SimpleYesNo).confirm(&ScriptedPrompter::always(Answer::Yes));
        assert!(yes.is_ok(), "{yes:?}");
        assert_eq!(
            pending(ConfirmationMode::SimpleYesNo).confirm(&ScriptedPrompter::always(Answer::No)).err(),
            Some(GuardRejection::Policy(PolicyError::AbortedByUser))
        );
        assert_eq!(
            pending(ConfirmationMode::SimpleYesNo).confirm(&ScriptedPrompter::always(Answer::NoTty)).err(),
            Some(GuardRejection::Policy(PolicyError::ConfirmationRequired))
        );
    }

    /// A plain "y" must never stand in for the typed word `PURGE`.
    #[test]
    fn a_yes_to_the_simple_question_does_not_satisfy_a_purge() {
        let prompter = ScriptedPrompter::new(Answer::Yes, Answer::No);
        let refused = pending(ConfirmationMode::TypedLiteral(PURGE_LITERAL)).confirm(&prompter);
        assert_eq!(refused.err(), Some(GuardRejection::Policy(PolicyError::AbortedByUser)));
        assert_eq!(prompter.confirms.load(Ordering::Relaxed), 0, "the y/N prompt is not used");
        assert_eq!(prompter.literals.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_stopping_mode_is_refused_without_asking_anybody() {
        let stopping = [
            (ConfirmationMode::RequiredButNoTty, PolicyError::ConfirmationRequired),
            (
                ConfirmationMode::Rejected(RejectReason::RedNotActionable),
                PolicyError::Rejected(RejectReason::RedNotActionable),
            ),
        ];
        for (mode, expected) in stopping {
            let prompter = ScriptedPrompter::always(Answer::Yes);
            let rejected = pending(mode).confirm(&prompter);
            assert_eq!(rejected.err(), Some(GuardRejection::Policy(expected)), "{mode:?}");
            assert_eq!(prompter.confirms.load(Ordering::Relaxed), 0, "{mode:?}");
            assert_eq!(prompter.literals.load(Ordering::Relaxed), 0, "{mode:?}");
        }
    }

    #[test]
    fn a_pending_approval_shows_what_it_is_waiting_for() {
        let waiting = pending(ConfirmationMode::DetailedExplicit);
        assert_eq!(waiting.mode(), ConfirmationMode::DetailedExplicit);
        assert!(waiting.plan().is_dry_run());
        assert_eq!(waiting.items().len(), 1);
        assert_eq!(waiting.request().item_count, 0);
        let prompter = ScriptedPrompter::always(Answer::Yes);
        let approved = waiting.confirm(&prompter).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(approved.plan().items().len(), 0);
        assert_eq!(prompter.confirms.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_token_describes_what_it_authorises_without_leaking_the_plan() {
        let token: Approved<Write> = issue::<Write>(payload());
        assert_eq!(format!("{token:?}"), "Approved<clean plan execution>");
        assert!(token.plan().is_dry_run());
        assert_eq!(token.items()[0].inode(), 7);
        assert_eq!(token.into_plan().items().len(), 0);

        let entries: Approved<QuarantineWrite> =
            issue::<QuarantineWrite>(vec![evidence_for("/q/items/1", 2, 9)]);
        assert_eq!(entries.items().len(), 1);
        assert_eq!(entries.into_items()[0].path(), std::path::Path::new("/q/items/1"));
    }

    #[test]
    fn a_fresh_request_writes_nothing() {
        let request = WriteRequest::new("/Users/dana");
        assert!(!request.apply && !request.purge && !request.yes && !request.tty && !request.ci);
        assert_eq!(request.max_size, None);
        assert!(request.exclusions.is_empty());
        assert!(request.quarantine_root.is_none());
        let roots = request.allowed_roots().unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(roots.roots().len(), 6, "three roots, two spellings each");
    }

    #[test]
    fn a_request_with_an_impossible_home_cannot_produce_roots() {
        let request = WriteRequest::new("/");
        assert!(matches!(request.allowed_roots(), Err(GuardRejection::InvalidRoot { .. })));
    }
}
