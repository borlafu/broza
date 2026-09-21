//! Donation message gate (`docs/cli-spec.md` §5, RF-17).
//!
//! This module is a pure predicate: no I/O, no clock, no environment. The
//! caller reads the marker file, the clock and the terminal state and passes
//! the facts in; [`crate::donate_display`] does that and prints the message.

use jiff::{SignedDuration, Timestamp};

/// Ko-fi link shown by the message and by `broza about`.
pub const DONATE_URL: &str = "https://ko-fi.com/broza";
/// Marker file, relative to the user's home directory.
pub const MARKER_RELATIVE: &str = ".local/share/broza/state/donate_last_shown";
/// Minimum time between two messages (condition 6): 30 days.
/// Expressed in hours because `jiff::Timestamp` arithmetic takes no calendar units.
const COOLDOWN: SignedDuration = SignedDuration::from_hours(30 * 24);

/// Everything the six conditions of §5 depend on.
// The six conditions are booleans; collapsing them would hide the mapping.
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
    /// Condition 6: contents of the marker file, if it could be read.
    pub last_shown: Option<Timestamp>,
    /// Condition 6: the current time.
    pub now: Timestamp,
}

/// Decide whether the donation message may be shown.
///
/// All six conditions of `docs/cli-spec.md` §5 must hold. A missing or
/// unreadable marker counts as "never shown".
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
        && is_past_cooldown(input.last_shown, input.now)
}

/// Condition 6: at least [`COOLDOWN`] since the last message.
fn is_past_cooldown(last_shown: Option<Timestamp>, now: Timestamp) -> bool {
    let Some(last) = last_shown else {
        return true;
    };
    let Ok(cooldown_ends) = last.checked_add(COOLDOWN) else {
        return true;
    };
    now >= cooldown_ends
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn now() -> Timestamp {
        "2026-09-21T10:36:08Z".parse().unwrap()
    }

    /// An input where every one of the six conditions holds.
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
            last_shown: None,
            now: now(),
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
            (
                "6: shown yesterday",
                DonationInput {
                    last_shown: Some(now().checked_sub(SignedDuration::from_hours(24)).unwrap()),
                    ..eligible()
                },
            ),
        ];
        for (reason, input) in cases {
            assert!(!should_show_donation(&input), "condition `{reason}` must veto the message");
        }
    }

    #[test]
    fn a_marker_older_than_thirty_days_does_not_veto() {
        let input = DonationInput {
            last_shown: Some(now().checked_sub(SignedDuration::from_hours(31 * 24)).unwrap()),
            ..eligible()
        };
        assert!(should_show_donation(&input));
    }

    #[test]
    fn exactly_thirty_days_is_enough() {
        let input = DonationInput { last_shown: Some(now().checked_sub(COOLDOWN).unwrap()), ..eligible() };
        assert!(should_show_donation(&input));
    }

    #[test]
    fn an_unreadable_marker_counts_as_never_shown() {
        assert!(should_show_donation(&DonationInput { last_shown: None, ..eligible() }));
    }
}
