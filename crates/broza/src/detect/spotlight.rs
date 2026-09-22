//! Spotlight's "last used" date, asked through `mdls` on the process port.
//!
//! One question per path, and none after the first failure: a stuck or missing
//! `mdls` costs one timeout and one `spotlight_unavailable` warning, not one per
//! file. `kMDItemLastUsedDate` is what `LaunchServices` updates when an
//! application is opened, and what Finder shows as "Last opened"; for many
//! system files Spotlight has no value, which the callers state as low
//! confidence (`docs/cli-spec.md` §3.3).

use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;

use crate::detect::detector::first_line;
use crate::model::Diagnostic;
use crate::ports::ProcessRunner;

/// Spotlight's metadata query tool.
pub const MDLS: &str = "/usr/bin/mdls";
/// The one attribute Broza asks for, raw: one line, or `(null)`. The path
/// comes last; it is absolute, so it can never be read as an option.
pub const MDLS_ARGS: [&str; 3] = ["-name", "kMDItemLastUsedDate", "-raw"];
/// Warning code when Spotlight could not be asked at all.
pub const SPOTLIGHT_UNAVAILABLE_CODE: &str = "spotlight_unavailable";
/// `mdls` answers in milliseconds; a stuck Spotlight must not stall `suggest`.
const MDLS_TIMEOUT: Duration = Duration::from_secs(5);
/// What `mdls -raw` prints when the attribute is unset.
const MDLS_NULL: &str = "(null)";
/// How `mdls -raw` prints a date: `2026-09-21 10:48:47 +0000`.
const MDLS_DATE_FORMAT: &str = "%Y-%m-%d %H:%M:%S %z";

/// Spotlight, asked once per path until it fails once; after that no more.
pub struct Spotlight<'a> {
    process: &'a dyn ProcessRunner,
    /// What the warning describes, when it fires: "files" or "applications".
    subject: &'static str,
    /// Why `mdls` could not answer, the first time it failed.
    failure: Option<String>,
}

impl<'a> Spotlight<'a> {
    /// Ready to ask about `subject` (named in the warning, plural).
    pub fn new(process: &'a dyn ProcessRunner, subject: &'static str) -> Self {
        Self { process, subject, failure: None }
    }

    /// `kMDItemLastUsedDate` of `path`, when Spotlight has one.
    ///
    /// The path goes last and must be absolute: `mdls` has no `--`, and a
    /// relative name starting with `-` would read as an option.
    pub fn last_used(&mut self, path: &Path) -> Option<Timestamp> {
        if self.failure.is_some() || !path.is_absolute() {
            return None;
        }
        let path_text = path.to_str()?;
        let args: Vec<&str> = MDLS_ARGS.iter().copied().chain([path_text]).collect();
        let output = match self.process.run(MDLS, &args, MDLS_TIMEOUT) {
            Ok(output) if output.success => output,
            Ok(output) => {
                self.note_failure(&output.stderr_text());
                return None;
            }
            Err(error) => {
                self.note_failure(&error.to_string());
                return None;
            }
        };
        parse_mdls_date(output.stdout_text().trim())
    }

    fn note_failure(&mut self, reason: &str) {
        if self.failure.is_none() {
            self.failure = Some(first_line(reason));
        }
    }

    /// One warning for the whole run, when `mdls` failed at least once.
    pub fn warning(&self) -> Option<Diagnostic> {
        self.failure.as_ref().map(|reason| Diagnostic {
            code: SPOTLIGHT_UNAVAILABLE_CODE.to_owned(),
            message: format!(
                "Spotlight could not be asked when {} were last opened ({reason}); they were judged by \
                 the filesystem's times alone",
                self.subject
            ),
            path: None,
        })
    }
}

/// A date as `mdls -raw` prints it, or nothing for `(null)` and anything else.
pub fn parse_mdls_date(raw: &str) -> Option<Timestamp> {
    if raw.is_empty() || raw == MDLS_NULL {
        return None;
    }
    Timestamp::strptime(MDLS_DATE_FORMAT, raw).ok()
}

#[cfg(test)]
mod tests {
    use super::parse_mdls_date;

    #[test]
    fn mdls_dates_parse_and_null_does_not() {
        let expected = "2026-09-21T10:48:47Z".parse().ok();
        assert_eq!(parse_mdls_date("2026-09-21 10:48:47 +0000"), expected);
        assert_eq!(parse_mdls_date("(null)"), None);
        assert_eq!(parse_mdls_date(""), None);
        assert_eq!(parse_mdls_date("yesterday"), None);
    }
}
