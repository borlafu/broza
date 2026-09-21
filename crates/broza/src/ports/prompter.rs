//! Interactive confirmation.

use crate::model::Risk;

/// What the user is asked to confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmationRequest {
    /// Highest risk level in the plan.
    pub max_risk: Risk,
    /// Number of items to act on.
    pub item_count: usize,
    /// Total bytes affected.
    pub total_bytes: u64,
    /// `true` when the action is irreversible (`--purge`, `tmutil`).
    pub irreversible: bool,
    /// Paths to show; the prompter decides how many to print.
    pub preview: Vec<String>,
}

/// Outcome of a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// User confirmed.
    Yes,
    /// User declined (exit code 6).
    No,
    /// No interactive terminal available (exit code 7).
    NoTty,
}

/// Asks the user for confirmation on stderr/stdin.
pub trait Prompter: Send + Sync {
    /// Simple `y/N` prompt.
    fn confirm(&self, request: &ConfirmationRequest) -> Answer;
    /// Prompt that requires typing `expected` literally (for example `PURGE`).
    fn confirm_literal(&self, request: &ConfirmationRequest, expected: &str) -> Answer;
}
