//! Rendering of a command result into the selected [`OutputFormat`].

use broza::BrozaError;
use broza::model::Envelope;
use serde::Serialize;

use crate::output::format::OutputFormat;

/// A command result that can be rendered in every supported format.
///
/// Implementors own their human text; JSON always goes through the envelope so
/// the contract of `docs/cli-spec.md` §4 has a single implementation.
pub trait Renderer {
    /// Human-readable text, without a trailing newline.
    fn to_human(&self) -> String;

    /// Flat CSV table. Commands that do not support `--csv` are rejected in
    /// [`crate::cli::Cli::validate`] before rendering.
    ///
    /// # Errors
    ///
    /// [`BrozaError::Usage`] when the command has no tabular form.
    fn to_csv(&self) -> Result<String, BrozaError> {
        Err(BrozaError::Usage("this command has no CSV output".to_owned()))
    }

    /// The JSON payload, already wrapped in its envelope.
    ///
    /// # Errors
    ///
    /// [`BrozaError::Other`] when serialization fails.
    fn to_json(&self) -> Result<String, BrozaError>;

    /// Render in `format`.
    ///
    /// # Errors
    ///
    /// Whatever [`Renderer::to_json`] or [`Renderer::to_csv`] returns.
    fn render(&self, format: OutputFormat) -> Result<String, BrozaError> {
        match format {
            OutputFormat::Human => Ok(self.to_human()),
            OutputFormat::Json => self.to_json(),
            OutputFormat::Csv => self.to_csv(),
        }
    }
}

/// Serialize an envelope as pretty JSON (`docs/cli-spec.md` §4.1).
///
/// # Errors
///
/// [`BrozaError::Other`] when `serde_json` cannot serialize `envelope`.
pub fn envelope_to_json<T: Serialize>(envelope: &Envelope<T>) -> Result<String, BrozaError> {
    serde_json::to_string_pretty(envelope)
        .map_err(|source| BrozaError::Other(format!("cannot serialize JSON output: {source}")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use broza::model::Host;

    use super::*;

    struct Fixture;

    impl Renderer for Fixture {
        fn to_human(&self) -> String {
            "human".to_owned()
        }

        fn to_json(&self) -> Result<String, BrozaError> {
            envelope_to_json(&Envelope::new(
                "about",
                Host { macos_version: "26.1".into(), arch: "arm64".into() },
                "2026-09-21T10:36:08Z",
                serde_json::json!({ "ok": true }),
            ))
        }
    }

    #[test]
    fn human_rendering_returns_the_command_text() {
        assert_eq!(Fixture.render(OutputFormat::Human).unwrap(), "human");
    }

    #[test]
    fn json_rendering_is_pretty_and_carries_the_schema_version() {
        let rendered = Fixture.render(OutputFormat::Json).unwrap();
        assert!(rendered.contains("\"schema_version\": \"1.1\""), "{rendered}");
        assert!(rendered.contains('\n'), "pretty JSON must be multi-line");
    }

    #[test]
    fn csv_defaults_to_a_usage_error() {
        let err = Fixture.render(OutputFormat::Csv).expect_err("no CSV form");
        assert_eq!(broza::ExitCode::from(&err), broza::ExitCode::UsageError);
    }
}
