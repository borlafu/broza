//! Confirmation policy (`docs/cli-spec.md` §3.4, confirmation matrix).
//!
//! A pure function: no clock, no terminal, no environment. The caller collects the
//! six inputs and renders the resulting [`ConfirmationMode`]. The truth table is
//! covered exhaustively by a test against an independently written oracle.

use crate::model::Risk;

/// The literal word the user must type to confirm an irreversible purge.
pub const PURGE_LITERAL: &str = "PURGE";

/// Everything the confirmation matrix depends on.
///
/// The five flags are the rows of the matrix in `docs/cli-spec.md` §3.4; collapsing
/// them into enums would hide the correspondence with the specification, so the
/// `struct_excessive_bools` lint is allowed here deliberately.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyInput {
    /// `--apply` was given; without it nothing is ever written.
    pub apply: bool,
    /// Highest risk level in the plan; `None` for an empty plan.
    pub max_risk: Option<Risk>,
    /// `--purge` was given (irreversible deletion, bypassing quarantine).
    pub purge: bool,
    /// The plan destroys something for good, with or without `--purge`: emptying
    /// the trash and deleting a snapshot are irreversible by nature.
    pub irreversible: bool,
    /// `--yes` was given.
    pub yes: bool,
    /// stdin/stderr are attached to an interactive terminal.
    pub tty: bool,
    /// The `CI` environment variable is set; treated as "no TTY".
    pub ci: bool,
}

/// Why a plan is refused before any confirmation is even offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// The selection contains a red or `inform_only` item (`docs/cli-spec.md` §2).
    RedNotActionable,
    /// `--yes` combined with `--purge`; a purge never accepts an implicit "yes".
    YesWithPurge,
}

impl RejectReason {
    /// Human-readable explanation, used in error messages.
    pub const fn message(self) -> &'static str {
        match self {
            Self::RedNotActionable => {
                "the selection contains red or inform-only items, which Broza never removes"
            }
            Self::YesWithPurge => "--purge never accepts --yes; the word PURGE must be typed",
        }
    }
}

/// How (and whether) the user must confirm before the plan runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationMode {
    /// No prompt: dry run, `--yes`, or an empty plan.
    None,
    /// Summary plus a `y/N` prompt (green only).
    SimpleYesNo,
    /// Every path listed plus an explicit `y/N` prompt (amber present).
    DetailedExplicit,
    /// The user must type this literal word (`PURGE`).
    TypedLiteral(&'static str),
    /// The plan is refused; the caller never prompts.
    Rejected(RejectReason),
    /// Confirmation is required but no interactive terminal is available (exit 7).
    RequiredButNoTty,
}

impl ConfirmationMode {
    /// `true` when the caller must run a [`crate::ports::Prompter`].
    pub const fn needs_prompt(self) -> bool {
        matches!(self, Self::SimpleYesNo | Self::DetailedExplicit | Self::TypedLiteral(_))
    }
}

/// Decides the confirmation mode for one `clean --apply` invocation.
///
/// First match wins; the order of the rules is normative (`docs/cli-spec.md` §3.4).
pub fn confirmation_policy(input: PolicyInput) -> ConfirmationMode {
    let PolicyInput { apply, max_risk, purge, irreversible, yes, tty, ci } = input;
    if !apply {
        return ConfirmationMode::None;
    }
    if matches!(max_risk, Some(Risk::Red)) {
        return ConfirmationMode::Rejected(RejectReason::RedNotActionable);
    }
    if purge && yes {
        return ConfirmationMode::Rejected(RejectReason::YesWithPurge);
    }
    if purge && tty && !ci {
        return ConfirmationMode::TypedLiteral(PURGE_LITERAL);
    }
    if purge {
        return ConfirmationMode::RequiredButNoTty;
    }
    if yes {
        return ConfirmationMode::None;
    }
    if !tty || ci {
        return ConfirmationMode::RequiredButNoTty;
    }
    match max_risk {
        // An irreversible plan is never waved through with a one-line question,
        // even when every item is green.
        Some(Risk::Green) if !irreversible => ConfirmationMode::SimpleYesNo,
        Some(Risk::Green | Risk::Amber) => ConfirmationMode::DetailedExplicit,
        _ => ConfirmationMode::None,
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfirmationMode, PURGE_LITERAL, PolicyInput, RejectReason, confirmation_policy};
    use crate::model::Risk;

    const RISKS: [Option<Risk>; 4] = [None, Some(Risk::Green), Some(Risk::Amber), Some(Risk::Red)];

    /// Independent re-statement of the matrix, nested instead of flat, used only as a
    /// cross-check: two different shapes of the same rules must agree everywhere.
    fn oracle(input: PolicyInput) -> ConfirmationMode {
        if !input.apply {
            return ConfirmationMode::None;
        }
        if matches!(input.max_risk, Some(Risk::Red)) {
            return ConfirmationMode::Rejected(RejectReason::RedNotActionable);
        }
        let interactive = input.tty && !input.ci;
        if input.purge {
            if input.yes {
                return ConfirmationMode::Rejected(RejectReason::YesWithPurge);
            }
            return if interactive {
                ConfirmationMode::TypedLiteral(PURGE_LITERAL)
            } else {
                ConfirmationMode::RequiredButNoTty
            };
        }
        if input.yes {
            return ConfirmationMode::None;
        }
        if !interactive {
            return ConfirmationMode::RequiredButNoTty;
        }
        if matches!(input.max_risk, Some(Risk::Amber)) {
            return ConfirmationMode::DetailedExplicit;
        }
        if matches!(input.max_risk, Some(Risk::Green)) {
            return if input.irreversible {
                ConfirmationMode::DetailedExplicit
            } else {
                ConfirmationMode::SimpleYesNo
            };
        }
        ConfirmationMode::None
    }

    fn all_inputs() -> Vec<PolicyInput> {
        (0..64_u8)
            .flat_map(|bits| {
                RISKS.into_iter().map(move |max_risk| PolicyInput {
                    apply: bits & 1 != 0,
                    max_risk,
                    purge: bits & 2 != 0,
                    yes: bits & 4 != 0,
                    tty: bits & 8 != 0,
                    ci: bits & 16 != 0,
                    irreversible: bits & 32 != 0,
                })
            })
            .collect()
    }

    #[test]
    fn the_whole_truth_table_matches_the_oracle() {
        let inputs = all_inputs();
        assert_eq!(inputs.len(), 256, "2^6 flag combinations times 4 risk levels");
        for input in inputs {
            assert_eq!(confirmation_policy(input), oracle(input), "{input:?}");
        }
    }

    fn input(apply: bool, max_risk: Option<Risk>) -> PolicyInput {
        PolicyInput { apply, max_risk, purge: false, irreversible: false, yes: false, tty: true, ci: false }
    }

    #[test]
    fn a_dry_run_never_prompts() {
        for risk in RISKS {
            assert_eq!(confirmation_policy(input(false, risk)), ConfirmationMode::None);
        }
    }

    #[test]
    fn green_asks_a_simple_question_and_amber_a_detailed_one() {
        assert_eq!(confirmation_policy(input(true, Some(Risk::Green))), ConfirmationMode::SimpleYesNo);
        assert_eq!(confirmation_policy(input(true, Some(Risk::Amber))), ConfirmationMode::DetailedExplicit);
    }

    #[test]
    fn red_is_rejected_even_with_yes_and_a_terminal() {
        let red = PolicyInput { yes: true, ..input(true, Some(Risk::Red)) };
        assert_eq!(confirmation_policy(red), ConfirmationMode::Rejected(RejectReason::RedNotActionable));
    }

    #[test]
    fn purge_with_yes_is_a_usage_error() {
        let purge = PolicyInput { purge: true, yes: true, ..input(true, Some(Risk::Green)) };
        assert_eq!(confirmation_policy(purge), ConfirmationMode::Rejected(RejectReason::YesWithPurge));
    }

    #[test]
    fn purge_on_a_terminal_demands_the_literal_word() {
        let purge = PolicyInput { purge: true, ..input(true, Some(Risk::Amber)) };
        assert_eq!(confirmation_policy(purge), ConfirmationMode::TypedLiteral("PURGE"));
        assert!(confirmation_policy(purge).needs_prompt());
    }

    #[test]
    fn purge_without_a_terminal_needs_confirmation_it_cannot_get() {
        let purge = PolicyInput { purge: true, tty: false, ..input(true, Some(Risk::Green)) };
        assert_eq!(confirmation_policy(purge), ConfirmationMode::RequiredButNoTty);
        let in_ci = PolicyInput { purge: true, ci: true, ..input(true, Some(Risk::Green)) };
        assert_eq!(confirmation_policy(in_ci), ConfirmationMode::RequiredButNoTty);
    }

    #[test]
    fn ci_is_treated_as_no_terminal() {
        let in_ci = PolicyInput { ci: true, ..input(true, Some(Risk::Green)) };
        assert_eq!(confirmation_policy(in_ci), ConfirmationMode::RequiredButNoTty);
    }

    #[test]
    fn yes_skips_the_prompt_but_only_without_purge() {
        let yes = PolicyInput { yes: true, ..input(true, Some(Risk::Amber)) };
        assert_eq!(confirmation_policy(yes), ConfirmationMode::None);
        assert!(!confirmation_policy(yes).needs_prompt());
    }

    /// Emptying the trash or deleting a snapshot destroys data for good even
    /// without `--purge`, so the user sees every path before answering.
    #[test]
    fn a_green_but_irreversible_plan_is_shown_in_full() {
        let irreversible = PolicyInput { irreversible: true, ..input(true, Some(Risk::Green)) };
        assert_eq!(confirmation_policy(irreversible), ConfirmationMode::DetailedExplicit);
        assert_eq!(confirmation_policy(input(true, Some(Risk::Green))), ConfirmationMode::SimpleYesNo);
    }

    /// `--yes` is an explicit confirmation the user typed on the command line; it
    /// covers an irreversible plan, unlike `--purge`, which never accepts it.
    #[test]
    fn yes_still_covers_an_irreversible_plan_without_purge() {
        let irreversible = PolicyInput { irreversible: true, yes: true, ..input(true, Some(Risk::Amber)) };
        assert_eq!(confirmation_policy(irreversible), ConfirmationMode::None);
        let purging = PolicyInput { purge: true, ..irreversible };
        assert_eq!(confirmation_policy(purging), ConfirmationMode::Rejected(RejectReason::YesWithPurge));
    }

    #[test]
    fn an_empty_plan_needs_no_confirmation_when_a_terminal_is_present() {
        assert_eq!(confirmation_policy(input(true, None)), ConfirmationMode::None);
    }
}
