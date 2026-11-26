//! `broza doctor` (`docs/cli-spec.md` §3.9): is Full Disk Access on, which
//! application needs it, and where Broza keeps its own files.
//!
//! Read-only. `--open` asks macOS to show the Full Disk Access pane of System
//! Settings, which changes nothing by itself: the user still flips the switch.
//! The state is data, not an error, so the command exits `0` whatever it finds.

use std::path::{Path, PathBuf};
use std::time::Duration;

use broza::BrozaError;
use broza::access::check_full_disk_access;
use broza::model::{AccessCheck, AccessState, DoctorReport, Envelope, Host, Warning};
use broza::ports::Ports;
use jiff::Timestamp;

use crate::args::DoctorArgs;
use crate::commands::Outcome;
use crate::output::human::doctor as human;
use crate::output::{OutputFormat, Renderer, envelope_to_json};

/// The Full Disk Access pane of System Settings.
pub const SETTINGS_URL: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";
/// The program that opens it.
const OPEN_PROGRAM: &str = "open";
/// How long `open` may take before Broza gives up on it.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
/// Warning code: System Settings could not be opened.
pub const SETTINGS_NOT_OPENED_CODE: &str = "settings_not_opened";
/// The scan cache, relative to the home.
const CACHE_RELATIVE: &str = ".cache/broza";

/// Everything `run` needs beyond the arguments.
pub struct DoctorContext<'a> {
    /// The wired adapters.
    pub ports: &'a Ports,
    /// The `doctor` flags as parsed.
    pub args: &'a DoctorArgs,
    /// The home directory, when there is one.
    pub home: Option<&'a Path>,
    /// The terminal application, when `TERM_PROGRAM` named one Broza knows.
    pub terminal: Option<&'static str>,
    /// Where the configuration puts the quarantine store.
    pub quarantine_path: Option<PathBuf>,
    /// `host` block of the envelope.
    pub host: Host,
    /// `generated_at` of the envelope.
    pub generated_at: Timestamp,
    /// Warnings raised before the command ran.
    pub warnings: Vec<Warning>,
    /// Format the caller asked for.
    pub format: OutputFormat,
}

/// Check, optionally open System Settings, and render.
///
/// # Errors
///
/// Only a rendering failure; whatever the checks find is reported as data.
pub fn run(context: &DoctorContext<'_>) -> Result<Outcome, BrozaError> {
    let full_disk_access =
        context.home.map_or(AccessCheck { state: AccessState::Unknown, probe_path: None }, |home| {
            check_full_disk_access(context.ports.fs.as_ref(), home)
        });
    let (opened_settings, open_warning) =
        if context.args.open { open_settings(context.ports) } else { (false, None) };
    let report = DoctorReport {
        full_disk_access,
        terminal: context.terminal.map(str::to_owned),
        settings_url: SETTINGS_URL.to_owned(),
        opened_settings,
        quarantine_path: context.quarantine_path.clone(),
        cache_path: context.home.map(|home| home.join(CACHE_RELATIVE)),
    };
    let warnings: Vec<Warning> = context.warnings.iter().cloned().chain(open_warning).collect();
    let output = DoctorOutput { report, context, warnings: warnings.clone() };
    Ok(Outcome::ok(output.render(context.format)?).with_warnings(warnings))
}

/// Ask macOS to show the Full Disk Access pane; a failure is a warning.
fn open_settings(ports: &Ports) -> (bool, Option<Warning>) {
    let failure = match ports.process.run(OPEN_PROGRAM, &[SETTINGS_URL], OPEN_TIMEOUT) {
        Ok(output) if output.success => return (true, None),
        Ok(output) if output.stderr_text().trim().is_empty() => {
            format!(
                "`open` exited with {}",
                output.code.map_or_else(|| "a signal".to_owned(), |c| c.to_string())
            )
        }
        Ok(output) => output.stderr_text(),
        Err(error) => error.to_string(),
    };
    let warning = Warning {
        code: SETTINGS_NOT_OPENED_CODE.to_owned(),
        message: format!(
            "System Settings could not be opened ({}); open Privacy & Security → Full Disk Access yourself",
            failure.trim()
        ),
        path: None,
    };
    (false, Some(warning))
}

/// The report together with what the renderers need around it.
struct DoctorOutput<'a> {
    report: DoctorReport,
    context: &'a DoctorContext<'a>,
    warnings: Vec<Warning>,
}

impl Renderer for DoctorOutput<'_> {
    fn to_human(&self) -> String {
        human::render(&self.report, self.context.home)
    }

    fn to_json(&self) -> Result<String, BrozaError> {
        let envelope = Envelope::new(
            "doctor",
            self.context.host.clone(),
            self.context.generated_at,
            self.report.clone(),
        );
        let envelope = self.warnings.iter().cloned().fold(envelope, Envelope::with_warning);
        envelope_to_json(&envelope)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use broza::ports::ProcessOutput;

    use super::*;
    use crate::commands::test_world::{HOME, STORE, host, now, world};

    const PROBE: &str = "/System/Volumes/Data/Users/dana/Library/Application Support/com.apple.TCC";

    fn context<'a>(ports: &'a Ports, args: &'a DoctorArgs, format: OutputFormat) -> DoctorContext<'a> {
        DoctorContext {
            ports,
            args,
            home: Some(Path::new(HOME)),
            terminal: Some("Terminal"),
            quarantine_path: Some(PathBuf::from(STORE)),
            host: host(),
            generated_at: now(),
            warnings: Vec::new(),
            format,
        }
    }

    #[test]
    fn a_refused_protected_folder_is_reported_as_missing_with_the_steps() {
        let (ports, handles) = world();
        handles.fs.add_dir(PROBE);
        handles.fs.add_denied(PROBE);
        let args = DoctorArgs::default();

        let outcome = run(&context(&ports, &args, OutputFormat::Human)).unwrap();

        assert!(outcome.rendered.starts_with("Full Disk Access   missing"), "{}", outcome.rendered);
        assert!(outcome.rendered.contains("Turn on Terminal"), "{}", outcome.rendered);
        assert!(handles.process.calls().is_empty(), "nothing is opened without --open");
    }

    #[test]
    fn open_asks_macos_for_the_full_disk_access_pane() {
        let (ports, handles) = world();
        handles.fs.add_dir(PROBE);
        let success = ProcessOutput { success: true, code: Some(0), stdout: Vec::new(), stderr: Vec::new() };
        handles.process.script_output(OPEN_PROGRAM, &[SETTINGS_URL], success);
        let args = DoctorArgs { open: true };

        let outcome = run(&context(&ports, &args, OutputFormat::Json)).unwrap();

        let json: serde_json::Value = serde_json::from_str(&outcome.rendered).unwrap();
        assert_eq!(json["data"]["full_disk_access"]["state"], "granted");
        assert_eq!(json["data"]["opened_settings"], true);
        assert_eq!(json["data"]["terminal"], "Terminal");
        assert_eq!(handles.process.calls().len(), 1);
        assert!(outcome.warnings.is_empty());
    }

    #[test]
    fn a_settings_pane_that_will_not_open_is_a_warning_not_a_failure() {
        let (ports, handles) = world();
        handles.process.script_failure(OPEN_PROGRAM, &[SETTINGS_URL], "no window server");
        let args = DoctorArgs { open: true };

        let outcome = run(&context(&ports, &args, OutputFormat::Human)).unwrap();

        assert_eq!(outcome.code, broza::ExitCode::Ok);
        assert_eq!(outcome.warnings.len(), 1);
        assert_eq!(outcome.warnings[0].code, SETTINGS_NOT_OPENED_CODE);
        assert!(outcome.rendered.contains("unknown"), "{}", outcome.rendered);
    }
}
