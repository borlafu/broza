//! Common JSON envelope shared by every command (`docs/cli-spec.md` §4.1).

use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Envelope wrapping the `data` of every command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope<T> {
    /// Semver of the JSON contract.
    pub schema_version: String,
    /// Version of the Broza binary that produced the output.
    pub broza_version: String,
    /// When the output was produced, RFC 3339 UTC with whole seconds.
    #[serde(with = "crate::model::timestamp")]
    pub generated_at: Timestamp,
    /// Command that produced the output (`scan`, `suggest`, ...).
    pub command: String,
    /// Host information.
    pub host: Host,
    /// Command-specific payload.
    pub data: T,
    /// Non-fatal warnings.
    #[serde(default)]
    pub warnings: Vec<Warning>,
    /// Errors; non-empty implies a partial failure.
    #[serde(default)]
    pub errors: Vec<ErrorEntry>,
}

/// Host description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    /// macOS product version, e.g. `26.1`.
    pub macos_version: String,
    /// CPU architecture, e.g. `arm64`.
    pub arch: String,
}

/// One entry of `warnings[]` or `errors[]`; both have the same shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Stable machine-readable code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Path the entry refers to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

/// A non-fatal warning. Warnings never affect the exit code.
pub type Warning = Diagnostic;

/// An error entry. A non-empty `errors[]` implies exit code `5`.
pub type ErrorEntry = Diagnostic;

impl<T> Envelope<T> {
    /// Build an envelope for `command` around `data`.
    pub fn new(command: impl Into<String>, host: Host, generated_at: Timestamp, data: T) -> Self {
        Self {
            schema_version: crate::SCHEMA_VERSION.to_owned(),
            broza_version: crate::BROZA_VERSION.to_owned(),
            generated_at,
            command: command.into(),
            host,
            data,
            warnings: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Return a copy with an extra warning.
    #[must_use]
    pub fn with_warning(self, warning: Warning) -> Self {
        let mut warnings = self.warnings;
        warnings.push(warning);
        Self { warnings, ..self }
    }

    /// Return a copy with an extra error.
    #[must_use]
    pub fn with_error(self, error: ErrorEntry) -> Self {
        let mut errors = self.errors;
        errors.push(error);
        Self { errors, ..self }
    }

    /// `true` when `errors` is non-empty (exit code 5).
    pub fn is_partial_failure(&self) -> bool {
        !self.errors.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{Diagnostic, Envelope, Host};
    use jiff::Timestamp;

    fn host() -> Host {
        Host { macos_version: "26.1".into(), arch: "arm64".into() }
    }

    fn at() -> Timestamp {
        "2026-09-21T10:36:08Z".parse().unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn serializes_with_schema_version_and_empty_arrays() {
        let env = Envelope::new("about", host(), at(), serde_json::json!({}));
        let json = serde_json::to_value(&env).unwrap_or_default();
        assert_eq!(json["schema_version"], "1.1");
        assert_eq!(json["command"], "about");
        assert_eq!(json["generated_at"], "2026-09-21T10:36:08Z");
        assert_eq!(json["warnings"], serde_json::json!([]));
        assert_eq!(json["errors"], serde_json::json!([]));
    }

    #[test]
    fn with_error_marks_partial_failure_without_mutating_original() {
        let original = Envelope::new("clean", host(), at(), 0_u8);
        let with_err = original.clone().with_error(Diagnostic {
            code: "cross_volume".into(),
            message: "skipped".into(),
            path: None,
        });
        assert!(!original.is_partial_failure());
        assert!(with_err.is_partial_failure());
    }

    #[test]
    fn with_warning_appends_without_touching_the_exit_code() {
        let original = Envelope::new("scan", host(), at(), 0_u8);
        let with_warning = original.clone().with_warning(Diagnostic {
            code: "spotlight_unavailable".into(),
            message: "no last-used dates".into(),
            path: None,
        });
        assert!(original.warnings.is_empty());
        assert_eq!(with_warning.warnings.len(), 1);
        assert!(!with_warning.is_partial_failure());
    }

    #[test]
    fn deserializes_ignoring_unknown_fields() {
        let raw = r#"{"schema_version":"1.1","broza_version":"0.1.0","generated_at":"2026-09-21T10:36:08Z",
            "command":"about","host":{"macos_version":"26.1","arch":"arm64"},"data":{},"future_field":1}"#;
        let env: Envelope<serde_json::Value> = serde_json::from_str(raw).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(env.command, "about");
        assert_eq!(env.generated_at, at());
        assert!(env.warnings.is_empty());
    }

    #[test]
    fn a_malformed_generated_at_is_rejected() {
        let raw = r#"{"schema_version":"1.1","broza_version":"0.1.0","generated_at":"yesterday",
            "command":"about","host":{"macos_version":"26.1","arch":"arm64"},"data":{}}"#;
        assert!(serde_json::from_str::<Envelope<serde_json::Value>>(raw).is_err());
    }
}
