//! The guard: the only place in Broza that can produce an [`Approved`] token.
//!
//! `docs/adr/0003-approved-token-safety-kernel.md`. Every mutating function takes
//! `&Approved<_>`; `grep -rn "Approved<" crates/` therefore enumerates every write
//! path in the code base.
//!
//! The flow of one `clean --apply` is:
//!
//! ```text
//! approve(plan, request, mounts, fs)   checks 1-7 of docs/cli-spec.md 3.4
//!   -> Verdict::DryRun                 nothing may be written
//!   -> Verdict::NeedsConfirmation(p)   p.confirm(prompter) or p.seal(answer)
//!        -> Approved<Write>            the executor's ticket
//! ```

mod checks;

pub use checks::{approve, approve_quarantine_write, narrow_to_snapshot_delete};

use std::fmt;
use std::marker::PhantomData;
use std::path::PathBuf;

use crate::model::{CleanPlan, Risk};
use crate::ports::{Answer, ConfirmationRequest, Prompter};
use crate::safety::exclusions::Exclusions;
use crate::safety::path::AllowedRoots;
use crate::safety::policy::{ConfirmationMode, PolicyInput};
use crate::safety::rejection::{GuardRejection, PolicyError};

/// Types only nameable inside this module; they are what makes the token unforgeable.
mod seal {
    /// Private field of [`super::Approved`]: no other module can write it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Seal;

    /// Supertrait that stops foreign implementations of [`super::WriteKind`].
    pub trait Sealed {}
}

/// What an [`Approved`] token authorises.
pub trait WriteKind: seal::Sealed {
    /// What the token carries to the executor.
    type Payload;
    /// Short description, used in `Debug` output.
    const DESCRIPTION: &'static str;
}

/// Execution of a clean plan (quarantine or purge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Write;
/// A write inside the quarantine store (restore, expire, purge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuarantineWrite;
/// Deletion of APFS local snapshots through `tmutil`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotDelete;

impl seal::Sealed for Write {}
impl seal::Sealed for QuarantineWrite {}
impl seal::Sealed for SnapshotDelete {}

impl WriteKind for Write {
    type Payload = CleanPlan;
    const DESCRIPTION: &'static str = "clean plan execution";
}

impl WriteKind for QuarantineWrite {
    type Payload = Vec<PathBuf>;
    const DESCRIPTION: &'static str = "quarantine store write";
}

impl WriteKind for SnapshotDelete {
    type Payload = CleanPlan;
    const DESCRIPTION: &'static str = "snapshot deletion";
}

/// Proof that the safety kernel approved a write. Cannot be constructed elsewhere.
pub struct Approved<K: WriteKind> {
    payload: K::Payload,
    /// Never read: its type is the point. Only `guard` can name it, so only
    /// `guard` can build this struct (`docs/adr/0003-...` "private unit-struct seal").
    #[allow(dead_code)]
    seal: seal::Seal,
    kind: PhantomData<fn() -> K>,
}

impl<K: WriteKind> fmt::Debug for Approved<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Approved<{}>", K::DESCRIPTION)
    }
}

impl<K: WriteKind<Payload = CleanPlan>> Approved<K> {
    /// The approved plan.
    pub fn plan(&self) -> &CleanPlan {
        &self.payload
    }

    /// Consumes the token and yields the plan.
    pub fn into_plan(self) -> CleanPlan {
        self.payload
    }
}

impl Approved<QuarantineWrite> {
    /// The approved paths, all inside the quarantine store.
    pub fn paths(&self) -> &[PathBuf] {
        &self.payload
    }

    /// Consumes the token and yields the paths.
    pub fn into_paths(self) -> Vec<PathBuf> {
        self.payload
    }
}

/// Builds a token. The only constructor of [`Approved`] in the whole crate.
fn issue<K: WriteKind>(payload: K::Payload) -> Approved<K> {
    Approved { payload, seal: seal::Seal, kind: PhantomData }
}

/// Everything the guard needs to know about the invocation.
///
/// The five flags mirror the flags of `broza clean`; see
/// [`crate::safety::policy::PolicyInput`] for why they are not enums.
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
    /// Highest risk in the selection the plan was built from; `None` for an empty plan.
    pub max_risk: Option<Risk>,
    /// `--max-size` cap in bytes.
    pub max_size: Option<u64>,
    /// Exclusions from the configuration and the command line.
    pub exclusions: Exclusions,
    /// The user's home directory.
    pub home: PathBuf,
    /// Per-uid temporary directories (`/private/var/folders/<xx>/<uid dir>`).
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
            max_risk: None,
            max_size: None,
            exclusions: Exclusions::none(),
            home: home.into(),
            uid_temp_dirs: Vec::new(),
        }
    }

    fn policy_input(&self) -> PolicyInput {
        PolicyInput {
            apply: self.apply,
            max_risk: self.max_risk,
            purge: self.purge,
            yes: self.yes,
            tty: self.tty,
            ci: self.ci,
        }
    }

    fn allowed_roots(&self) -> AllowedRoots {
        AllowedRoots::new(&self.home, self.uid_temp_dirs.clone())
    }
}

/// What the guard decided.
#[derive(Debug)]
pub enum Verdict {
    /// No `--apply`: the plan is reported and nothing is written.
    DryRun(CleanPlan),
    /// Every safety check passed; the confirmation still has to happen.
    NeedsConfirmation(PendingApproval),
}

/// A plan that passed every check and is waiting for the user's answer.
#[derive(Debug)]
pub struct PendingApproval {
    plan: CleanPlan,
    mode: ConfirmationMode,
    request: ConfirmationRequest,
    /// Never read; see [`Approved`].
    #[allow(dead_code)]
    seal: seal::Seal,
}

impl PendingApproval {
    /// The plan that will be executed once confirmed.
    pub fn plan(&self) -> &CleanPlan {
        &self.plan
    }

    /// How the user must confirm.
    pub fn mode(&self) -> ConfirmationMode {
        self.mode
    }

    /// What to show the user.
    pub fn request(&self) -> &ConfirmationRequest {
        &self.request
    }

    /// Turns the user's answer into a token.
    ///
    /// `Yes` produces the token, `No` aborts (exit `6`) and `NoTty` means the
    /// confirmation could not be obtained (exit `7`).
    pub fn seal(self, answer: Answer) -> Result<Approved<Write>, GuardRejection> {
        match answer {
            Answer::Yes => Ok(issue::<Write>(self.plan)),
            Answer::No => Err(PolicyError::AbortedByUser.into()),
            Answer::NoTty => Err(PolicyError::ConfirmationRequired.into()),
        }
    }

    /// Runs the prompt this plan requires and seals the answer.
    pub fn confirm(self, prompter: &dyn Prompter) -> Result<Approved<Write>, GuardRejection> {
        let answer = match self.mode {
            ConfirmationMode::None => Answer::Yes,
            ConfirmationMode::SimpleYesNo | ConfirmationMode::DetailedExplicit => {
                prompter.confirm(&self.request)
            }
            ConfirmationMode::TypedLiteral(word) => prompter.confirm_literal(&self.request, word),
            stopping => return Err(stop(stopping)),
        };
        self.seal(answer)
    }
}

/// Maps a confirmation mode that stops the operation to its rejection.
fn stop(mode: ConfirmationMode) -> GuardRejection {
    PolicyError::try_from(mode).map_or_else(
        |mode| GuardRejection::Inconsistent(format!("{mode:?} does not stop the plan")),
        Into::into,
    )
}

#[cfg(test)]
mod tests {
    use super::{Approved, PendingApproval, QuarantineWrite, Write, WriteRequest, issue, seal};
    use crate::model::{CleanPlan, Risk, SessionId};
    use crate::ports::{Answer, ConfirmationRequest};
    use crate::safety::policy::ConfirmationMode;
    use crate::safety::rejection::{GuardRejection, PolicyError};

    fn session() -> SessionId {
        "cln_20260921103608_a1b2".parse().unwrap_or_else(|error| panic!("{error}"))
    }

    fn plan() -> CleanPlan {
        CleanPlan::dry_run(session(), Vec::new()).unwrap_or_else(|error| panic!("{error}"))
    }

    fn pending(mode: ConfirmationMode) -> PendingApproval {
        PendingApproval {
            plan: plan(),
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

    #[test]
    fn yes_produces_a_token_and_no_aborts() {
        let approved = pending(ConfirmationMode::SimpleYesNo).seal(Answer::Yes);
        assert!(approved.is_ok(), "{approved:?}");
        assert_eq!(
            pending(ConfirmationMode::SimpleYesNo).seal(Answer::No).err(),
            Some(GuardRejection::Policy(PolicyError::AbortedByUser))
        );
        assert_eq!(
            pending(ConfirmationMode::SimpleYesNo).seal(Answer::NoTty).err(),
            Some(GuardRejection::Policy(PolicyError::ConfirmationRequired))
        );
    }

    #[test]
    fn a_stopping_mode_can_never_be_confirmed() {
        struct NeverAsked;
        impl crate::ports::Prompter for NeverAsked {
            fn confirm(&self, _request: &ConfirmationRequest) -> Answer {
                panic!("the guard must not prompt for a stopping mode")
            }
            fn confirm_literal(&self, _request: &ConfirmationRequest, _expected: &str) -> Answer {
                panic!("the guard must not prompt for a stopping mode")
            }
        }
        let rejected = pending(ConfirmationMode::RequiredButNoTty).confirm(&NeverAsked);
        assert_eq!(rejected.err(), Some(GuardRejection::Policy(PolicyError::ConfirmationRequired)));
    }

    #[test]
    fn a_token_describes_what_it_authorises_without_leaking_the_plan() {
        let token: Approved<Write> = issue::<Write>(plan());
        assert_eq!(format!("{token:?}"), "Approved<clean plan execution>");
        assert!(token.plan().is_dry_run());
        assert_eq!(token.into_plan().items().len(), 0);
        let paths: Approved<QuarantineWrite> = issue::<QuarantineWrite>(vec!["/q/items/1".into()]);
        assert_eq!(paths.paths().len(), 1);
        assert_eq!(paths.into_paths(), vec![std::path::PathBuf::from("/q/items/1")]);
    }

    #[test]
    fn a_fresh_request_writes_nothing() {
        let request = WriteRequest::new("/Users/dana");
        assert!(!request.apply && !request.purge && !request.yes && !request.tty && !request.ci);
        assert_eq!(request.max_size, None);
        assert!(request.exclusions.is_empty());
        assert_eq!(request.allowed_roots().roots().len(), 6, "three roots, two spellings each");
    }
}
