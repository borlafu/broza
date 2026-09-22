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
use crate::output::OutputFormat;

/// Warning code the mount table and the adapters use for a path macOS refused.
pub const PERMISSION_DENIED_CODE: &str = "permission_denied";
/// The one hint that follows a run which had to skip something (§6).
pub const FULL_DISK_ACCESS_HINT: &str = "Some locations were skipped. Grant Full Disk Access: \
System Settings → Privacy & Security → Full Disk Access, then add your terminal application (or \
the broza binary).";

/// Warnings go to stderr; `--quiet` and the machine-readable formats silence them.
///
/// When any of them is a permission refusal the run ends with one hint rather
/// than with one hint per path: missing Full Disk Access is a single fact
/// about the machine, and repeating it per skipped volume is noise.
pub fn report_warnings(warnings: &[Warning], global: &GlobalArgs, format: OutputFormat) {
    if global.quiet || format.is_machine_readable() {
        return;
    }
    let mut stderr = std::io::stderr();
    for line in warning_lines(warnings) {
        let _ignored = writeln!(stderr, "{line}");
    }
}

/// Exactly what [`report_warnings`] would write, as lines.
///
/// Split out so the shape can be tested: one line per warning, then at most
/// one Full Disk Access hint however many paths were refused.
pub fn warning_lines(warnings: &[Warning]) -> Vec<String> {
    let reported = warnings.iter().map(|warning| format!("warning: {}", warning.message));
    let hint = needs_full_disk_access(warnings).then(|| FULL_DISK_ACCESS_HINT.to_owned());
    reported.chain(hint).collect()
}

/// `true` when at least one warning is a permission refusal.
pub fn needs_full_disk_access(warnings: &[Warning]) -> bool {
    warnings.iter().any(|warning| warning.code == PERMISSION_DENIED_CODE)
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

        let lines = warning_lines(&warnings);

        assert_eq!(lines.len(), 4, "three warnings and one hint: {lines:?}");
        assert_eq!(lines.iter().filter(|line| line.contains("Full Disk Access")).count(), 1);
        assert_eq!(lines.last().map(String::as_str), Some(FULL_DISK_ACCESS_HINT), "the hint comes last");
    }

    #[test]
    fn without_a_permission_warning_nothing_extra_is_printed() {
        let lines = warning_lines(&[warning("host_version_unknown")]);

        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("warning: "), "{lines:?}");
        assert!(warning_lines(&[]).is_empty());
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
        report_warnings(&warnings, &loud, OutputFormat::Human);
        report_warnings(&warnings, &hushed, OutputFormat::Human);
        report_warnings(&warnings, &loud, OutputFormat::Json);
        assert_eq!(report_error(&BrozaError::Usage("x".into()), &hushed), ExitCode::UsageError);
    }
}
