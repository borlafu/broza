//! Everything Broza says on stderr: warnings, errors, and the one hint that
//! turns a wall of skipped paths into an action the user can take.
//!
//! stdout is data and stderr is conversation (`docs/cli-spec.md` §0,
//! principle 5). `--quiet` silences the conversation but never an error, and a
//! `--json` or `--csv` run says nothing here at all, because its warnings
//! travel inside the envelope where a program can read them.

use std::error::Error as _;
use std::io::Write as _;

use broza::model::Warning;
use broza::{BrozaError, ExitCode};
use clap::error::ErrorKind;

use crate::cli::GlobalArgs;
use crate::output::style::{Style, paint};
use crate::output::wrap::{hang, wrap};
use crate::output::{ColorPolicy, OutputFormat};

/// Warning code the mount table and the adapters use for a path macOS refused.
pub const PERMISSION_DENIED_CODE: &str = "permission_denied";
/// Label in front of every warning.
const WARNING_LABEL: &str = "warning:";
/// Hanging indent of a wrapped warning: the label's width plus its space.
const WARNING_INDENT: &str = "         ";
/// The one hint that follows a run which had to skip something (§6).
pub const FULL_DISK_ACCESS_HINT: &str = "Some locations were skipped. Grant Full Disk Access: \
System Settings → Privacy & Security → Full Disk Access, then add your terminal application (or \
the broza binary).";

/// Warnings go to stderr; `--quiet` and the machine-readable formats silence them.
///
/// When any of them is a permission refusal the run ends with one hint rather
/// than with one hint per path: missing Full Disk Access is a single fact
/// about the machine, and repeating it per skipped volume is noise. `policy`
/// must already account for stderr being a terminal.
pub fn report_warnings(warnings: &[Warning], global: &GlobalArgs, format: OutputFormat, policy: ColorPolicy) {
    if global.quiet || format.is_machine_readable() || warnings.is_empty() {
        return;
    }
    // Best effort: a closed stderr must not change the exit code.
    let _ignored = std::io::stderr().write_all(warning_block(warnings, policy).as_bytes());
}

/// Exactly what [`report_warnings`] writes: a blank line to set the block
/// apart from the report, then each warning wrapped under its label, a blank
/// line between two, then at most one Full Disk Access hint however many
/// paths were refused. Empty when there is nothing to say.
pub fn warning_block(warnings: &[Warning], policy: ColorPolicy) -> String {
    let label = paint(policy, Style::Yellow, WARNING_LABEL);
    let reported = warnings.iter().map(|warning| {
        hang(WARNING_LABEL, &warning.message, WARNING_INDENT).replacen(WARNING_LABEL, &label, 1)
    });
    let hint = needs_full_disk_access(warnings).then(|| wrap(FULL_DISK_ACCESS_HINT, ""));
    let paragraphs: Vec<String> = reported.chain(hint).collect();
    if paragraphs.is_empty() {
        return String::new();
    }
    format!("\n{}\n", paragraphs.join("\n\n"))
}

/// `true` when at least one warning is a permission refusal, one at a time or
/// collapsed into a summary.
pub fn needs_full_disk_access(warnings: &[Warning]) -> bool {
    warnings.iter().any(|warning| {
        warning.code == PERMISSION_DENIED_CODE || warning.code == broza::scan::PERMISSION_SUMMARY_CODE
    })
}

/// `--help` and `--version` are successes; every other clap error is exit `2`.
pub fn report_clap_error(error: &clap::Error) -> ExitCode {
    let is_help_or_version = matches!(error.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion);
    let _ignored = error.print();
    if is_help_or_version { ExitCode::Ok } else { ExitCode::UsageError }
}

/// Report `error` on stderr (principle 5) and map it to its exit code.
///
/// Errors are never silenced by `--quiet`; `-v` adds the source chain.
pub fn report_error(error: &BrozaError, global: &GlobalArgs) -> ExitCode {
    let mut stderr = std::io::stderr();
    let _ignored = writeln!(stderr, "error: {error}");
    if global.verbose > 0 {
        let mut source = error.source();
        while let Some(cause) = source {
            let _ignored = writeln!(stderr, "  caused by: {cause}");
            source = cause.source();
        }
    }
    // A refusal that ended the command gets the same hint a skipped path does
    // (§6): the fix is the same setting.
    if matches!(error, BrozaError::PermissionDenied { .. }) {
        let _ignored = writeln!(stderr, "{FULL_DISK_ACCESS_HINT}");
    }
    ExitCode::from(error)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use clap::Parser;

    use super::*;
    use crate::cli::Cli;

    fn warning(code: &str) -> Warning {
        Warning { code: code.to_owned(), message: format!("something about {code}"), path: None }
    }

    fn quiet() -> GlobalArgs {
        GlobalArgs::default()
    }

    #[test]
    fn a_permission_warning_earns_the_full_disk_access_hint() {
        assert!(needs_full_disk_access(&[warning(PERMISSION_DENIED_CODE)]));
        assert!(
            needs_full_disk_access(&[warning(broza::scan::PERMISSION_SUMMARY_CODE)]),
            "the summary of many refusals is the case that needs the hint most"
        );
        assert!(needs_full_disk_access(&[warning("firmlinks_unreadable"), warning(PERMISSION_DENIED_CODE)]));
    }

    #[test]
    fn any_other_warning_does_not() {
        assert!(!needs_full_disk_access(&[]));
        assert!(!needs_full_disk_access(&[warning("host_version_unknown"), warning("folder_scan_pending")]));
    }

    #[test]
    fn the_hint_is_printed_once_however_many_paths_were_refused() {
        let warnings = vec![
            warning(PERMISSION_DENIED_CODE),
            warning("firmlinks_unreadable"),
            warning(PERMISSION_DENIED_CODE),
        ];

        let block = warning_block(&warnings, ColorPolicy::Never);

        let paragraphs: Vec<&str> = block.trim().split("\n\n").collect();
        assert_eq!(paragraphs.len(), 4, "three warnings and one hint: {block}");
        assert_eq!(block.matches("Full Disk Access").count(), 2, "once in the hint, which names it twice");
        assert!(paragraphs[3].starts_with("Some locations were skipped."), "the hint comes last: {block}");
    }

    #[test]
    fn without_a_permission_warning_nothing_extra_is_printed() {
        let block = warning_block(&[warning("host_version_unknown")], ColorPolicy::Never);

        assert_eq!(block, "\nwarning: something about host_version_unknown\n");
        assert!(warning_block(&[], ColorPolicy::Never).is_empty());
    }

    #[test]
    fn a_long_warning_wraps_under_its_label_and_stands_apart_from_the_report() {
        let long = Warning {
            code: "permission_denied_summary".to_owned(),
            message: "421 locations could not be read, so their sizes are missing from the totals. \
                      Examples: /System/Volumes/Data/.fseventsd, /System/Volumes/Data/.Spotlight-V100. \
                      Pass -v to list every path."
                .to_owned(),
            path: None,
        };

        let block = warning_block(&[long], ColorPolicy::Never);

        assert!(block.starts_with("\nwarning: 421 locations"), "a blank line first: {block:?}");
        let lines: Vec<&str> = block.lines().skip(1).take_while(|line| !line.is_empty()).collect();
        assert!(lines.len() > 1, "{block}");
        assert!(lines[1..].iter().all(|line| line.starts_with(WARNING_INDENT)), "hanging indent: {block}");
        assert!(lines.iter().all(|line| line.chars().count() <= crate::output::wrap::WRAP_WIDTH), "{block}");
    }

    #[test]
    fn an_empty_warning_message_prints_its_label_and_never_panics() {
        let empty = Warning { code: "x".to_owned(), message: String::new(), path: None };

        assert_eq!(warning_block(&[empty], ColorPolicy::Never), "\nwarning:\n");
    }

    #[test]
    fn only_the_label_is_coloured() {
        let block = warning_block(&[warning("host_version_unknown")], ColorPolicy::Always);

        assert!(block.starts_with("\n\u{1b}[33mwarning:\u{1b}[0m something"), "{block:?}");
    }

    #[test]
    fn the_hint_names_the_setting_the_user_has_to_open() {
        assert!(FULL_DISK_ACCESS_HINT.contains("Full Disk Access"));
        assert!(FULL_DISK_ACCESS_HINT.contains("System Settings"));
        assert!(FULL_DISK_ACCESS_HINT.contains("Privacy & Security"));
    }

    #[test]
    fn help_and_version_are_successes() {
        let help = Cli::try_parse_from(["broza", "--help"]).expect_err("clap returns an error");
        assert_eq!(report_clap_error(&help), ExitCode::Ok);
        let version = Cli::try_parse_from(["broza", "--version"]).expect_err("clap returns an error");
        assert_eq!(report_clap_error(&version), ExitCode::Ok);
    }

    #[test]
    fn unknown_flags_are_usage_errors() {
        let error = Cli::try_parse_from(["broza", "--nope"]).expect_err("must fail");
        assert_eq!(report_clap_error(&error), ExitCode::UsageError);
    }

    #[test]
    fn core_errors_keep_their_exit_code() {
        assert_eq!(report_error(&BrozaError::Usage("x".into()), &quiet()), ExitCode::UsageError);
        assert_eq!(report_error(&BrozaError::ConfirmationRequired, &quiet()), ExitCode::ConfirmationRequired);
        assert_eq!(report_error(&BrozaError::Other("x".into()), &quiet()), ExitCode::GenericError);
    }

    #[test]
    fn verbose_does_not_change_the_exit_code_of_a_sourced_error() {
        let error = BrozaError::Io {
            context: "writing".to_owned(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        };
        let verbose = GlobalArgs { verbose: 2, ..GlobalArgs::default() };
        assert_eq!(report_error(&error, &verbose), ExitCode::GenericError);
        assert_eq!(report_error(&error, &quiet()), ExitCode::GenericError);
    }

    #[test]
    fn quiet_silences_warnings_but_never_errors() {
        let warnings = vec![warning(PERMISSION_DENIED_CODE)];
        let loud = GlobalArgs::default();
        let hushed = GlobalArgs { quiet: true, ..GlobalArgs::default() };
        // Exercises every branch; the visible effect is stderr only, so the
        // assertion is that none of them panics or changes an exit code.
        report_warnings(&warnings, &loud, OutputFormat::Human, ColorPolicy::Never);
        report_warnings(&warnings, &hushed, OutputFormat::Human, ColorPolicy::Never);
        report_warnings(&warnings, &loud, OutputFormat::Json, ColorPolicy::Never);
        assert_eq!(report_error(&BrozaError::Usage("x".into()), &hushed), ExitCode::UsageError);
    }
}
