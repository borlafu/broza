//! Donation message gate (`docs/cli-spec.md` §5, RF-17).
//!
//! This module is a pure predicate: no I/O, no environment. The caller reads
//! the terminal state and the flags and passes the facts in;
//! [`crate::donate_display`] does that and prints the message.

/// Ko-fi link shown by the message and by `broza about`.
pub const DONATE_URL: &str = "https://ko-fi.com/borlafu";

/// Everything the five conditions of §5 depend on.
// The five conditions are booleans; collapsing them would hide the mapping.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DonationInput {
    /// Condition 1: an `--apply` cleanup succeeded and moved or freed at least one byte.
    pub apply_succeeded: bool,
    /// Condition 1: bytes quarantined plus bytes freed.
    pub affected_bytes: u64,
    /// Condition 2: stdout is an interactive terminal.
    pub stdout_is_tty: bool,
    /// Condition 2: stderr is an interactive terminal.
    pub stderr_is_tty: bool,
    /// Condition 3: `--json` was given.
    pub json: bool,
    /// Condition 3: `--csv` was given.
    pub csv: bool,
    /// Condition 3: `--quiet` was given.
    pub quiet: bool,
    /// Condition 4: the `donate-prompt` configuration key.
    pub donate_prompt: bool,
    /// Condition 5: `BROZA_NO_DONATE` is set.
    pub broza_no_donate: bool,
    /// Condition 5: `CI` is set.
    pub ci: bool,
}

/// Decide whether the donation message may be shown.
///
/// All five conditions of `docs/cli-spec.md` §5 must hold.
pub fn should_show_donation(input: &DonationInput) -> bool {
    input.apply_succeeded
        && input.affected_bytes > 0
        && input.stdout_is_tty
        && input.stderr_is_tty
        && !input.json
        && !input.csv
        && !input.quiet
        && input.donate_prompt
        && !input.broza_no_donate
        && !input.ci
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An input where every one of the five conditions holds.
    fn eligible() -> DonationInput {
        DonationInput {
            apply_succeeded: true,
            affected_bytes: 138_200_000_000,
            stdout_is_tty: true,
            stderr_is_tty: true,
            json: false,
            csv: false,
            quiet: false,
            donate_prompt: true,
            broza_no_donate: false,
            ci: false,
        }
    }

    #[test]
    fn shows_when_every_condition_holds() {
        assert!(should_show_donation(&eligible()));
    }

    #[test]
    fn every_single_condition_can_veto_the_message() {
        let cases: Vec<(&str, DonationInput)> = vec![
            ("1: not an --apply run", DonationInput { apply_succeeded: false, ..eligible() }),
            ("1: nothing was affected", DonationInput { affected_bytes: 0, ..eligible() }),
            ("2: stdout is not a TTY", DonationInput { stdout_is_tty: false, ..eligible() }),
            ("2: stderr is not a TTY", DonationInput { stderr_is_tty: false, ..eligible() }),
            ("3: --json", DonationInput { json: true, ..eligible() }),
            ("3: --csv", DonationInput { csv: true, ..eligible() }),
            ("3: --quiet", DonationInput { quiet: true, ..eligible() }),
            ("4: donate-prompt false", DonationInput { donate_prompt: false, ..eligible() }),
            ("5: BROZA_NO_DONATE", DonationInput { broza_no_donate: true, ..eligible() }),
            ("5: CI", DonationInput { ci: true, ..eligible() }),
        ];
        for (reason, input) in cases {
            assert!(!should_show_donation(&input), "condition `{reason}` must veto the message");
        }
    }
}
