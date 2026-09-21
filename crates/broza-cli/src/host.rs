//! Host description for the JSON envelope (`docs/cli-spec.md` §4.1).
//!
//! The real provider asks `sw_vers -productVersion`, and it asks it through the
//! [`ProcessRunner`] port like every other command Broza runs: the port owns
//! the timeout and the process group, and a test can script the answer instead
//! of spawning anything (`AGENTS.md` §4). Tests that do not care about the
//! version use [`FixedHost`], which is also what `BROZA_HOST` selects in debug
//! builds. When the version cannot be determined the provider says so through a
//! `host_version_unknown` warning rather than quietly inventing a value.

use std::sync::Arc;
use std::time::Duration;

use broza::model::{Host, Warning};
use broza::ports::{ProcessOutput, ProcessRunner};

/// How long `sw_vers` may take before Broza gives up on it.
const SW_VERS_TIMEOUT: Duration = Duration::from_secs(2);
/// Reported macOS version when the system cannot be asked.
const UNKNOWN_VERSION: &str = "unknown";
/// Stable warning code emitted when the macOS version is not available.
pub const WARNING_HOST_VERSION_UNKNOWN: &str = "host_version_unknown";
/// Separator of the `BROZA_HOST` override, `<macos_version>/<arch>`.
const OVERRIDE_SEPARATOR: char = '/';
/// Executable asked for the product version; never resolved through `PATH`.
pub const SW_VERS: &str = "/usr/bin/sw_vers";
/// Arguments that make `sw_vers` print the product version and nothing else.
pub const SW_VERS_ARGS: [&str; 1] = ["-productVersion"];

/// A host description plus the warning it may have produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostReport {
    /// The `host` block of the envelope.
    pub host: Host,
    /// Non-fatal warning to add to `warnings[]`, if any.
    pub warning: Option<Warning>,
}

impl HostReport {
    /// A report with no warning.
    pub const fn certain(host: Host) -> Self {
        Self { host, warning: None }
    }
}

/// Source of the `host` block of the envelope.
pub trait HostInfo {
    /// Describe the machine Broza runs on.
    fn report(&self) -> HostReport;
}

/// Map Rust's architecture name to the value used by the JSON contract (`arm64`).
fn spec_arch(rust_arch: &str) -> &str {
    match rust_arch {
        "aarch64" => "arm64",
        other => other,
    }
}

/// Reads the architecture at compile time and the macOS version from `sw_vers`.
#[derive(Clone)]
pub struct SystemHost {
    /// Runs `sw_vers` with the timeout the port enforces.
    runner: Arc<dyn ProcessRunner>,
}

impl std::fmt::Debug for SystemHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SystemHost { .. }")
    }
}

impl SystemHost {
    /// A provider that asks `sw_vers` through `runner`.
    pub const fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self { runner }
    }
}

impl HostInfo for SystemHost {
    fn report(&self) -> HostReport {
        let arch = spec_arch(std::env::consts::ARCH).to_owned();
        match product_version(self.runner.as_ref()) {
            Ok(macos_version) => HostReport::certain(Host { macos_version, arch }),
            Err(reason) => HostReport {
                host: Host { macos_version: UNKNOWN_VERSION.to_owned(), arch },
                warning: Some(Warning {
                    code: WARNING_HOST_VERSION_UNKNOWN.to_owned(),
                    message: format!("could not determine the macOS version: {reason}"),
                    path: None,
                }),
            },
        }
    }
}

/// A host fixed at construction: used by tests and by the `BROZA_HOST` override.
#[derive(Debug, Clone)]
pub struct FixedHost(Host);

impl FixedHost {
    /// Build a provider that always reports `host`.
    pub const fn new(host: Host) -> Self {
        Self(host)
    }

    /// Parse a `<macos_version>/<arch>` override, for example `26.1/arm64`.
    pub fn parse(raw: &str) -> Option<Self> {
        let (version, arch) = raw.split_once(OVERRIDE_SEPARATOR)?;
        if version.is_empty() || arch.is_empty() {
            return None;
        }
        Some(Self(Host { macos_version: version.to_owned(), arch: arch.to_owned() }))
    }
}

impl HostInfo for FixedHost {
    fn report(&self) -> HostReport {
        HostReport::certain(self.0.clone())
    }
}

/// Pick the provider: the override when present and well-formed, else `runner`.
pub fn provider(override_value: Option<&str>, runner: Arc<dyn ProcessRunner>) -> Box<dyn HostInfo> {
    match override_value.and_then(FixedHost::parse) {
        Some(fixed) => Box::new(fixed),
        None => Box::new(SystemHost::new(runner)),
    }
}

/// Run `sw_vers -productVersion`; the port enforces [`SW_VERS_TIMEOUT`].
///
/// Returns why it failed so the caller can put it in a warning.
fn product_version(runner: &dyn ProcessRunner) -> Result<String, String> {
    let output = runner
        .run(SW_VERS, &SW_VERS_ARGS, SW_VERS_TIMEOUT)
        .map_err(|error| format!("{SW_VERS} could not be run: {error}"))?;
    interpret(&output)
}

/// Accept the output only when the process succeeded and said something.
fn interpret(output: &ProcessOutput) -> Result<String, String> {
    if !output.success {
        let code = output.code.map_or_else(|| "a signal".to_owned(), |code| format!("status {code}"));
        return Err(format!("{SW_VERS} exited with {code}"));
    }
    let text = output.stdout_text();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(format!("{SW_VERS} returned no version"));
    }
    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use broza::ports::ProcessOutput;
    use broza::testing::FakeRunner;

    use super::*;

    fn output(success: bool, code: Option<i32>, stdout: &str) -> ProcessOutput {
        ProcessOutput { success, code, stdout: stdout.as_bytes().to_vec(), stderr: Vec::new() }
    }

    fn answering(out: ProcessOutput) -> Arc<dyn ProcessRunner> {
        Arc::new(FakeRunner::new().with_output(SW_VERS, &SW_VERS_ARGS, out))
    }

    #[test]
    fn rust_arch_maps_to_contract_arch() {
        assert_eq!(spec_arch("aarch64"), "arm64");
        assert_eq!(spec_arch("x86_64"), "x86_64");
    }

    #[test]
    fn override_is_parsed_into_version_and_arch() {
        let report = FixedHost::parse("26.1/arm64").expect("valid override").report();
        assert_eq!(report.host.macos_version, "26.1");
        assert_eq!(report.host.arch, "arm64");
        assert_eq!(report.warning, None);
    }

    #[test]
    fn malformed_overrides_are_ignored() {
        for raw in ["", "26.1", "/arm64", "26.1/"] {
            assert!(FixedHost::parse(raw).is_none(), "{raw} must be rejected");
        }
    }

    #[test]
    fn provider_prefers_a_valid_override_over_running_anything() {
        let runner = Arc::new(FakeRunner::new());

        let report = provider(Some("27.0/x86_64"), Arc::clone(&runner) as Arc<dyn ProcessRunner>).report();

        assert_eq!(report.host.macos_version, "27.0");
        assert_eq!(report.host.arch, "x86_64");
        assert!(runner.calls().is_empty(), "the override must not spawn sw_vers");
    }

    #[test]
    fn fixed_host_reports_exactly_what_it_was_given() {
        let expected = Host { macos_version: "26.2".into(), arch: "arm64".into() };
        assert_eq!(FixedHost::new(expected.clone()).report().host, expected);
    }

    #[test]
    fn the_version_is_asked_for_through_the_process_port() {
        let runner =
            Arc::new(FakeRunner::new().with_output(SW_VERS, &SW_VERS_ARGS, output(true, Some(0), "26.1\n")));

        let report = provider(None, Arc::clone(&runner) as Arc<dyn ProcessRunner>).report();

        assert_eq!(report.host.macos_version, "26.1");
        assert_eq!(report.warning, None);
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].program, SW_VERS);
        assert_eq!(calls[0].args, vec!["-productVersion".to_owned()]);
        assert_eq!(calls[0].timeout, SW_VERS_TIMEOUT);
    }

    #[test]
    fn a_successful_run_yields_the_trimmed_version() {
        assert_eq!(interpret(&output(true, Some(0), "26.1\n")), Ok("26.1".to_owned()));
        assert_eq!(interpret(&output(true, Some(0), "  27.0  ")), Ok("27.0".to_owned()));
    }

    #[test]
    fn a_failed_run_or_blank_output_is_reported_as_a_reason() {
        assert!(interpret(&output(false, Some(1), "26.1")).is_err());
        assert!(interpret(&output(false, None, "26.1")).is_err());
        assert!(interpret(&output(true, Some(0), "")).is_err());
        assert!(interpret(&output(true, Some(0), "  \n")).is_err());
    }

    #[test]
    fn an_unavailable_sw_vers_becomes_a_warning_and_not_a_failure() {
        let report = SystemHost::new(answering(output(true, Some(0), ""))).report();

        assert_eq!(report.host.macos_version, UNKNOWN_VERSION);
        assert_eq!(report.warning.map(|w| w.code), Some(WARNING_HOST_VERSION_UNKNOWN.to_owned()));
    }

    #[test]
    fn a_runner_that_cannot_start_the_process_is_also_only_a_warning() {
        let runner = FakeRunner::new().with_failure(SW_VERS, &SW_VERS_ARGS, "no such file");

        let report = SystemHost::new(Arc::new(runner)).report();

        assert_eq!(report.host.macos_version, UNKNOWN_VERSION);
        let message = report.warning.map(|w| w.message).unwrap_or_default();
        assert!(message.contains("no such file"), "{message}");
    }
}
