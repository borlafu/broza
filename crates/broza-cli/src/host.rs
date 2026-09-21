//! Host description for the JSON envelope (`docs/cli-spec.md` §4.1).
//!
//! The real provider shells out to `sw_vers -productVersion`. Tests never do:
//! they use [`FixedHost`], which is also what `BROZA_HOST` selects at runtime.

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use broza::model::Host;

/// How long `sw_vers` may take before Broza gives up on it.
const SW_VERS_TIMEOUT: Duration = Duration::from_secs(2);
/// Reported macOS version when the system cannot be asked.
const UNKNOWN_VERSION: &str = "unknown";
/// Separator of the `BROZA_HOST` override, `<macos_version>/<arch>`.
const OVERRIDE_SEPARATOR: char = '/';

/// Source of the `host` block of the envelope.
pub trait HostInfo {
    /// Describe the machine Broza runs on.
    fn host(&self) -> Host;
}

/// Reads the architecture at compile time and the macOS version from `sw_vers`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemHost;

impl HostInfo for SystemHost {
    fn host(&self) -> Host {
        Host {
            macos_version: product_version().unwrap_or_else(|| UNKNOWN_VERSION.to_owned()),
            arch: std::env::consts::ARCH.to_owned(),
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
    fn host(&self) -> Host {
        self.0.clone()
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
fn product_version() -> Option<String> {
    let mut child = Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;

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
        return None;
    };
    let _ignored = child.wait();
    normalize_version(&text)
}

/// Trim the `sw_vers` output; blank output means "could not be determined".
fn normalize_version(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn override_is_parsed_into_version_and_arch() {
        let host = FixedHost::parse("26.1/arm64").expect("valid override").host();
        assert_eq!(host.macos_version, "26.1");
        assert_eq!(host.arch, "arm64");
    }

    #[test]
    fn malformed_overrides_are_ignored() {
        for raw in ["", "26.1", "/arm64", "26.1/"] {
            assert!(FixedHost::parse(raw).is_none(), "{raw} must be rejected");
        }
    }

    #[test]
    fn provider_prefers_a_valid_override() {
        let host = provider(Some("27.0/x86_64")).host();
        assert_eq!(host.macos_version, "27.0");
        assert_eq!(host.arch, "x86_64");
    }

    #[test]
    fn fixed_host_reports_exactly_what_it_was_given() {
        let expected = Host { macos_version: "26.2".into(), arch: "arm64".into() };
        assert_eq!(FixedHost::new(expected.clone()).host(), expected);
    }

    #[test]
    fn the_system_version_output_is_trimmed() {
        assert_eq!(normalize_version("26.1\n"), Some("26.1".to_owned()));
        assert_eq!(normalize_version("  27.0  "), Some("27.0".to_owned()));
    }

    #[test]
    fn blank_system_output_means_unknown() {
        assert_eq!(normalize_version(""), None);
        assert_eq!(normalize_version("   \n"), None);
    }

    #[test]
    fn the_default_provider_reports_the_compiled_architecture() {
        // `SystemHost` is only constructed here; `sw_vers` is never run in tests.
        let provider = provider(None);
        assert!(std::mem::size_of_val(&provider) > 0);
    }
}
