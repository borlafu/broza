//! The human rendering of `broza doctor` (`docs/cli-spec.md` §3.9).
//!
//! The verdict first, then what it means for a scan, then — when access is
//! not known to be on — the steps that turn it on, numbered, in the order the
//! user takes them. Pure: a report in, a `String` out.

use std::fmt::Write as _;
use std::path::Path;

use broza::model::{AccessState, DoctorReport};

use crate::output::human::scan::folders::abbreviate;
use crate::output::wrap::{numbered, wrap};

/// Indent of the paragraphs under a heading.
const INDENT: &str = "  ";
/// Width of the labels in the storage block.
const LABEL_WIDTH: usize = 12;
/// What to call the terminal when `TERM_PROGRAM` did not say.
const ANY_TERMINAL: &str = "your terminal application";

/// Render `report`; paths under `home` print as `~/…`.
pub fn render(report: &DoctorReport, home: Option<&Path>) -> String {
    let access = &report.full_disk_access;
    let mut text = format!("Full Disk Access   {}", state_word(access.state));
    let probe = access.probe_path.as_deref().map(|path| abbreviate(path, home));
    let _ignored = write!(text, "\n{}", wrap(&meaning(access.state, probe.as_deref()), INDENT));
    if report.opened_settings {
        let _ignored =
            write!(text, "\n\n{INDENT}System Settings is open at Privacy & Security → Full Disk Access.");
    }
    if access.state != AccessState::Granted {
        let app = report.terminal.as_deref().unwrap_or(ANY_TERMINAL);
        let _ignored = write!(text, "\n\n{INDENT}To turn it on:\n{}", numbered(&steps(app), INDENT));
    }
    let _ignored = write!(text, "\n\nBroza's own files");
    for (label, path) in [("Quarantine", &report.quarantine_path), ("Scan cache", &report.cache_path)] {
        if let Some(path) = path {
            let _ignored = write!(text, "\n{INDENT}{label:<LABEL_WIDTH$}{}", abbreviate(path, home));
        }
    }
    text
}

/// The verdict in one word.
const fn state_word(state: AccessState) -> &'static str {
    match state {
        AccessState::Granted => "granted",
        AccessState::Missing => "missing",
        _ => "unknown",
    }
}

/// What the verdict means for a scan.
fn meaning(state: AccessState, probe: Option<&str>) -> String {
    let probe = probe.unwrap_or("a protected folder");
    match state {
        AccessState::Granted => "Broza can read the folders macOS protects. A few system folders \
            (.Spotlight-V100, .fseventsd, .DocumentRevisions-V100) stay unreadable even so: they belong \
            to macOS, and scans leave them out. That is expected."
            .to_owned(),
        AccessState::Missing => format!(
            "macOS refused to let Broza list {probe}. Without Full Disk Access, scans leave out Mail, \
             Messages, Safari and other protected data, and their sizes are missing from the totals."
        ),
        _ => "Broza could not tell: none of the protected folders it checks could be examined. If \
              scans report many unreadable locations, Full Disk Access is probably off."
            .to_owned(),
    }
}

/// The steps that turn Full Disk Access on for `app`.
fn steps(app: &str) -> [String; 4] {
    [
        "Run `broza doctor --open`, or open System Settings → Privacy & Security → Full Disk Access."
            .to_owned(),
        format!("Turn on {app}. Not in the list? Click +, then add it from the Applications folder."),
        format!("Quit {app} and open it again: macOS applies the change to new processes only."),
        "Run `broza doctor` again to check.".to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use broza::model::AccessCheck;

    use super::*;

    fn report(state: AccessState, terminal: Option<&str>) -> DoctorReport {
        DoctorReport {
            full_disk_access: AccessCheck {
                state,
                probe_path: Some(PathBuf::from("/Users/dana/Library/Application Support/com.apple.TCC")),
            },
            terminal: terminal.map(str::to_owned),
            settings_url: String::new(),
            opened_settings: false,
            quarantine_path: Some(PathBuf::from("/Users/dana/.local/share/broza/quarantine")),
            cache_path: Some(PathBuf::from("/Users/dana/.cache/broza")),
        }
    }

    #[test]
    fn missing_access_names_the_folder_the_app_and_every_step() {
        let text = render(&report(AccessState::Missing, Some("iTerm")), Some(Path::new("/Users/dana")));

        assert!(
            text.starts_with("Full Disk Access   missing\n  macOS refused to let Broza list ~/Library"),
            "{text}"
        );
        assert!(text.contains("  1. Run `broza doctor --open`"), "{text}");
        assert!(text.contains("  2. Turn on iTerm."), "{text}");
        assert!(text.contains("  3. Quit iTerm and open it again"), "{text}");
        assert!(text.contains("  4. Run `broza doctor` again to check."), "{text}");
        assert!(text.ends_with("  Scan cache  ~/.cache/broza"), "{text}");
    }

    #[test]
    fn granted_access_explains_the_folders_that_stay_unreadable_and_gives_no_steps() {
        let text = render(&report(AccessState::Granted, None), None);

        assert!(text.starts_with("Full Disk Access   granted"), "{text}");
        assert!(text.contains(".Spotlight-V100"), "{text}");
        assert!(!text.contains("To turn it on"), "{text}");
    }

    #[test]
    fn an_unknown_terminal_is_named_generically() {
        let text = render(&report(AccessState::Unknown, None), None);

        assert!(text.contains("Turn on your terminal application."), "{text}");
    }
}
