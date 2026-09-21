//! Host description for the JSON envelope (`docs/cli-spec.md` §4.1).
//!
//! The real provider shells out to `sw_vers -productVersion`. Tests never do:
//! they use [`FixedHost`], which is also what `BROZA_HOST` selects in debug
//! builds. When the version cannot be determined the provider says so through a
//! `host_version_unknown` warning rather than quietly inventing a value.
//!
// TODO(M1): move behind the `ProcessRunner` port in `broza::adapters/` so the
// CLI stops spawning processes directly and the fake runner covers this path.

use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use broza::model::{Host, Warning};

/// How long `sw_vers` may take before Broza gives up on it.
const SW_VERS_TIMEOUT: Duration = Duration::from_secs(2);
/// Reported macOS version when the system cannot be asked.
const UNKNOWN_VERSION: &str = "unknown";
/// Stable warning code emitted when the macOS version is not available.
pub const WARNING_HOST_VERSION_UNKNOWN: &str = "host_version_unknown";
/// Separator of the `BROZA_HOST` override, `<macos_version>/<arch>`.
const OVERRIDE_SEPARATOR: char = '/';
/// Executable asked for the product version.
const SW_VERS: &str = "/usr/bin/sw_vers";

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
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemHost;

impl HostInfo for SystemHost {
    fn report(&self) -> HostReport {
        let arch = spec_arch(std::env::consts::ARCH).to_owned();
        match product_version() {
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

/// Pick the provider: the override when present and well-formed, else the system.
pub fn provider(override_value: Option<&str>) -> Box<dyn HostInfo> {
    match override_value.and_then(FixedHost::parse) {
        Some(fixed) => Box::new(fixed),
        None => Box::new(SystemHost),
    }
}

/// Run `sw_vers -productVersion`, giving up after [`SW_VERS_TIMEOUT`].
///
/// Returns why it failed so the caller can put it in a warning.
fn product_version() -> Result<String, String> {
    let mut child = Command::new(SW_VERS)
        .arg("-productVersion")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| format!("{SW_VERS} could not be started: {source}"))?;
    let Some(mut stdout) = child.stdout.take() else {
        let _ignored = child.kill();
        let _ignored = child.wait();
        return Err(format!("{SW_VERS} produced no output stream"));
    };

    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;

        let mut buffer = String::new();
        let read = stdout.read_to_string(&mut buffer);
        let _ignored = sender.send(read.ok().map(|_| buffer));
    });

    let Ok(Some(text)) = receiver.recv_timeout(SW_VERS_TIMEOUT) else {
        let _ignored = child.kill();
        let _ignored = child.wait();
        return Err(format!("{SW_VERS} did not answer within {} s", SW_VERS_TIMEOUT.as_secs()));
    };
    let status = child.wait().map_err(|source| format!("{SW_VERS} could not be awaited: {source}"))?;
    interpret(status, &text)
}

/// Accept the output only when the process succeeded and said something.
fn interpret(status: ExitStatus, raw: &str) -> Result<String, String> {
    if !status.success() {
        return Err(format!("{SW_VERS} exited with {status}"));
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("{SW_VERS} returned no version"));
    }
    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::os::unix::process::ExitStatusExt;

    use super::*;

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
    fn provider_prefers_a_valid_override() {
        let report = provider(Some("27.0/x86_64")).report();
        assert_eq!(report.host.macos_version, "27.0");
        assert_eq!(report.host.arch, "x86_64");
    }

    #[test]
    fn fixed_host_reports_exactly_what_it_was_given() {
        let expected = Host { macos_version: "26.2".into(), arch: "arm64".into() };
        assert_eq!(FixedHost::new(expected.clone()).report().host, expected);
    }

    #[test]
    fn a_successful_run_yields_the_trimmed_version() {
        assert_eq!(interpret(ExitStatus::from_raw(0), "26.1\n"), Ok("26.1".to_owned()));
        assert_eq!(interpret(ExitStatus::from_raw(0), "  27.0  "), Ok("27.0".to_owned()));
    }

    #[test]
    fn a_failed_run_or_blank_output_is_reported_as_a_reason() {
        // Raw 256 is "exited with status 1" in the wait(2) encoding.
        assert!(interpret(ExitStatus::from_raw(256), "26.1").is_err());
        assert!(interpret(ExitStatus::from_raw(0), "").is_err());
        assert!(interpret(ExitStatus::from_raw(0), "  \n").is_err());
    }
}
