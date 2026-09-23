//! Showing the donation message once the gate says yes (`docs/cli-spec.md` §5).
//!
//! [`crate::donate`] is the pure predicate; this module gathers its inputs from
//! the run (the outcome, the flags, the environment) and prints the banner on
//! stderr: a blank line, a dim rule, the first line in bold, the second line
//! dim, a dim rule, a blank line. Bold and dim follow the color policy, so a
//! run without color gets the same characters and no escapes.

use std::io::Write;

use crate::cli::GlobalArgs;
use crate::commands::{Outcome, Reclaimed};
use crate::donate::{DONATE_URL, DonationInput, should_show_donation};
use crate::env::RuntimeEnv;
use crate::output::color::ColorPolicy;
use crate::output::style::{Style, paint};
use crate::output::{OutputFormat, format_bytes};

/// Indent of every line of the banner.
const INDENT: &str = "  ";
/// The character the rules above and below the message are drawn with.
const RULE: char = '─';
/// Length of each rule, in characters, after the indent.
const RULE_WIDTH: usize = 78;

/// What a run has to have been for the message to be considered at all.
pub struct Run<'a> {
    /// The command's outcome, with its reclaim figures when it applied a cleanup.
    pub outcome: &'a Outcome,
    /// The global flags (`--json`, `--csv`, `--quiet`).
    pub global: &'a GlobalArgs,
    /// The environment (TTYs, `CI`, `BROZA_NO_DONATE`).
    pub runtime: &'a RuntimeEnv,
    /// The `donate-prompt` configuration key.
    pub donate_prompt: bool,
    /// Format the caller asked for.
    pub format: OutputFormat,
    /// Whether bold and dim may be used.
    pub policy: ColorPolicy,
}

/// Print the banner on `stderr` when every condition holds. The writer is a
/// parameter so tests can assert the lines themselves.
pub fn maybe_show(run: &Run<'_>, stderr: &mut dyn Write) {
    let Some(reclaimed) = run.outcome.reclaimed.as_ref() else { return };
    let input = DonationInput {
        apply_succeeded: run.outcome.code == broza::ExitCode::Ok,
        affected_bytes: reclaimed.total(),
        stdout_is_tty: run.runtime.stdout_is_tty,
        stderr_is_tty: run.runtime.stderr_is_tty,
        json: run.format == OutputFormat::Json,
        csv: run.format == OutputFormat::Csv,
        quiet: run.global.quiet,
        donate_prompt: run.donate_prompt,
        broza_no_donate: run.runtime.broza_no_donate,
        ci: run.runtime.ci,
    };
    if !should_show_donation(&input) {
        return;
    }
    // Best effort: a closed stderr must not change the exit code.
    let _ignored = stderr.write_all(banner(reclaimed, run.policy).as_bytes());
}

/// The banner of §5: the two lines between rules, set apart by blank lines.
pub fn banner(reclaimed: &Reclaimed, policy: ColorPolicy) -> String {
    let [made, ask] = message(reclaimed);
    let rule = paint(policy, Style::Dim, &RULE.to_string().repeat(RULE_WIDTH));
    let lines = [rule.clone(), paint(policy, Style::Bold, &made), paint(policy, Style::Dim, &ask), rule];
    let body = lines.map(|line| format!("{INDENT}{line}")).join("\n");
    format!("\n{body}\n\n")
}

/// The two lines of §5, unstyled. Pending and freed bytes are named apart; a
/// figure that is zero is left out of the parenthesis, never the other one with it.
pub fn message(reclaimed: &Reclaimed) -> [String; 2] {
    let parts: Vec<String> =
        [(reclaimed.quarantined_bytes, "in quarantine"), (reclaimed.freed_bytes, "freed")]
            .into_iter()
            .filter(|(bytes, _)| *bytes > 0)
            .map(|(bytes, what)| format!("{} {what}", format_bytes(bytes)))
            .collect();
    let detail = if parts.is_empty() { String::new() } else { format!(" ({})", parts.join(", ")) };
    [
        format!(
            "Broza made {} reclaimable{detail}. It is free and open source software.",
            format_bytes(reclaimed.total())
        ),
        format!("If it helped you: {DONATE_URL}   ·   Silence this: broza config set donate-prompt false"),
    ]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::PathBuf;

    use super::*;

    fn reclaimed(quarantined: u64, freed: u64) -> Reclaimed {
        Reclaimed { quarantined_bytes: quarantined, freed_bytes: freed }
    }

    #[test]
    fn the_message_reports_pending_and_freed_bytes_separately_and_drops_a_zero() {
        let both = message(&reclaimed(121_400_000_000, 16_800_000_000));
        let pending_only = message(&reclaimed(94_200_000_000, 0));
        let freed_only = message(&reclaimed(0, 12_400_000_000));

        assert_eq!(
            both[0],
            "Broza made 138.2 GB reclaimable (121.4 GB in quarantine, 16.8 GB freed). It is free and open source software."
        );
        assert_eq!(
            both[1],
            "If it helped you: https://ko-fi.com/borlafu   ·   Silence this: broza config set donate-prompt false"
        );
        assert_eq!(
            pending_only[0],
            "Broza made 94.2 GB reclaimable (94.2 GB in quarantine). It is free and open source software."
        );
        assert_eq!(
            freed_only[0],
            "Broza made 12.4 GB reclaimable (12.4 GB freed). It is free and open source software."
        );
    }

    /// The banner as a terminal without color shows it.
    fn plain_banner() -> String {
        let rule = "─".repeat(78);
        format!(
            "\n  {rule}\n  Broza made 12.4 GB reclaimable (12.4 GB freed). It is free and open source software.\n  If it helped you: https://ko-fi.com/borlafu   ·   Silence this: broza config set donate-prompt false\n  {rule}\n\n"
        )
    }

    #[test]
    fn without_color_the_banner_is_the_two_lines_between_rules_and_blank_lines() {
        assert_eq!(banner(&reclaimed(0, 12_400_000_000), ColorPolicy::Never), plain_banner());
    }

    #[test]
    fn with_color_the_banner_shows_the_same_characters_in_bold_and_dim() {
        let styled = banner(&reclaimed(0, 12_400_000_000), ColorPolicy::Always);

        let visible = styled.replace("\u{1b}[1m", "").replace("\u{1b}[2m", "").replace("\u{1b}[0m", "");
        assert_eq!(visible, plain_banner());
        assert!(styled.contains("\u{1b}[1mBroza made"), "the first line is bold: {styled:?}");
        assert!(styled.contains("\u{1b}[2mIf it helped"), "the second line is dim: {styled:?}");
        assert_eq!(styled.matches("\u{1b}[2m─").count(), 2, "both rules are dim: {styled:?}");
        for line in styled.lines().filter(|line| !line.is_empty()) {
            assert!(
                line.starts_with("  \u{1b}[") && line.ends_with("\u{1b}[0m"),
                "styled end to end: {line:?}"
            );
        }
    }

    fn interactive() -> RuntimeEnv {
        RuntimeEnv {
            stdout_is_tty: true,
            stderr_is_tty: true,
            stdin_is_tty: true,
            ..RuntimeEnv::for_tests(PathBuf::from("/Users/dana"))
        }
    }

    fn global() -> GlobalArgs {
        GlobalArgs {
            json: false,
            csv: false,
            output: None,
            no_color: false,
            quiet: false,
            verbose: 0,
            config: None,
            profile: None,
            no_cache: false,
        }
    }

    fn run<'a>(
        outcome: &'a Outcome,
        global: &'a GlobalArgs,
        runtime: &'a RuntimeEnv,
        format: OutputFormat,
    ) -> Run<'a> {
        Run { outcome, global, runtime, donate_prompt: true, format, policy: ColorPolicy::Never }
    }

    #[test]
    fn every_eligible_run_shows_the_banner_with_no_cooldown_between_them() {
        let outcome = Outcome::ok(String::new()).with_reclaimed(reclaimed(10, 0));
        let global = global();
        let runtime = interactive();
        let mut shown = Vec::new();

        maybe_show(&run(&outcome, &global, &runtime, OutputFormat::Human), &mut shown);
        maybe_show(&run(&outcome, &global, &runtime, OutputFormat::Human), &mut shown);

        let text = String::from_utf8(shown).unwrap();
        assert_eq!(text.matches("ko-fi.com/borlafu").count(), 2, "shown on both runs: {text}");
        assert_eq!(text, format!("{0}{0}", banner(&reclaimed(10, 0), ColorPolicy::Never)));
    }

    #[test]
    fn a_run_that_reclaimed_nothing_or_was_not_interactive_shows_nothing() {
        let dry = Outcome::ok(String::new());
        let applied = Outcome::ok(String::new()).with_reclaimed(reclaimed(10, 0));
        let global = global();
        let quiet_env = RuntimeEnv::for_tests(PathBuf::from("/Users/dana"));
        let interactive_env = interactive();
        let mut out = Vec::new();

        maybe_show(&run(&dry, &global, &interactive_env, OutputFormat::Human), &mut out);
        maybe_show(&run(&applied, &global, &quiet_env, OutputFormat::Human), &mut out);
        maybe_show(&run(&applied, &global, &interactive_env, OutputFormat::Json), &mut out);

        assert!(out.is_empty(), "no line for a dry run, a non-interactive run or --json: {out:?}");
    }
}
