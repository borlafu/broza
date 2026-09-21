//! `broza about` (`docs/cli-spec.md` §3.7).
//!
//! Prints version, license, schema version and the support link. It never
//! shows the donation prompt: it *is* the place for the link.

use broza::BrozaError;
use broza::model::{Envelope, Host, Warning};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::donate::DONATE_URL;
use crate::output::{Renderer, envelope_to_json};

/// Product name as shown to the user.
pub const NAME: &str = "Broza";
/// License identifier.
pub const LICENSE: &str = "MIT";
/// Source repository.
pub const HOMEPAGE: &str = "https://github.com/borlafu/broza";
/// Supported platforms, one line.
const PLATFORMS: &str = "Safe disk cleanup for macOS 26/27 on Apple Silicon.";

/// `data` payload of `broza about --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AboutData {
    /// Product name.
    pub name: String,
    /// Binary version.
    pub version: String,
    /// Version of the JSON contract.
    pub schema_version: String,
    /// License identifier.
    pub license: String,
    /// Source repository.
    pub homepage: String,
    /// Ko-fi link.
    pub donate_url: String,
}

impl Default for AboutData {
    fn default() -> Self {
        Self {
            name: NAME.to_owned(),
            version: broza::BROZA_VERSION.to_owned(),
            schema_version: broza::SCHEMA_VERSION.to_owned(),
            license: LICENSE.to_owned(),
            homepage: HOMEPAGE.to_owned(),
            donate_url: DONATE_URL.to_owned(),
        }
    }
}

/// Rendered `about` output.
#[derive(Debug, Clone)]
pub struct About {
    data: AboutData,
    host: Host,
    generated_at: Timestamp,
    warnings: Vec<Warning>,
}

impl About {
    /// Build the command result for `host` at `generated_at`; the core
    /// serialises it as RFC 3339 UTC with whole seconds.
    pub fn new(host: Host, generated_at: Timestamp, warnings: Vec<Warning>) -> Self {
        Self { data: AboutData::default(), host, generated_at, warnings }
    }
}

impl Renderer for About {
    fn to_human(&self) -> String {
        let AboutData { name, version, schema_version, license, homepage, donate_url } = &self.data;
        format!(
            "{name} {version}  ·  {license} License  ·  JSON schema {schema_version}\n\
             {PLATFORMS}\n\
             Source:   {homepage}\n\
             Support:  {donate_url}  (donation, nothing in return)"
        )
    }

    fn to_json(&self) -> Result<String, BrozaError> {
        let envelope = Envelope::new("about", self.host.clone(), self.generated_at, self.data.clone());
        let envelope = self.warnings.iter().cloned().fold(envelope, Envelope::with_warning);
        envelope_to_json(&envelope)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn at() -> Timestamp {
        "2026-09-21T10:36:08Z".parse().unwrap_or_else(|e| panic!("{e}"))
    }

    fn about() -> About {
        About::new(Host { macos_version: "26.1".into(), arch: "arm64".into() }, at(), Vec::new())
    }

    #[test]
    fn human_output_matches_the_specification_sketch() {
        let text = about().to_human();
        assert!(text.starts_with("Broza 0.1.0  ·  MIT License  ·  JSON schema 1.1"), "{text}");
        assert!(text.contains(HOMEPAGE), "{text}");
        assert!(text.contains(DONATE_URL), "{text}");
        assert_eq!(text.lines().count(), 4, "{text}");
    }

    #[test]
    fn json_output_is_a_valid_envelope_with_the_documented_fields() {
        let rendered = about().to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["command"], "about");
        assert_eq!(parsed["schema_version"], broza::SCHEMA_VERSION);
        assert_eq!(parsed["host"]["macos_version"], "26.1");
        assert_eq!(parsed["generated_at"], "2026-09-21T10:36:08Z");
        for field in ["name", "version", "schema_version", "license", "homepage", "donate_url"] {
            assert!(parsed["data"][field].is_string(), "data.{field} must be present");
        }
    }

    #[test]
    fn about_has_no_csv_form() {
        assert!(about().to_csv().is_err());
    }

    #[test]
    fn a_host_warning_is_carried_into_the_envelope() {
        let with_warning = About::new(
            Host { macos_version: "unknown".into(), arch: "arm64".into() },
            at(),
            vec![Warning {
                code: crate::host::WARNING_HOST_VERSION_UNKNOWN.into(),
                message: "sw_vers failed".into(),
                path: None,
            }],
        );
        let parsed: serde_json::Value = serde_json::from_str(&with_warning.to_json().unwrap()).unwrap();
        assert_eq!(parsed["warnings"][0]["code"], "host_version_unknown");
        assert_eq!(parsed["host"]["macos_version"], "unknown");
    }
}
