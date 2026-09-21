//! `broza config get|set|list|path|reset` (`docs/cli-spec.md` §3.6).
//!
//! `get` and `list` report the **effective** configuration (file + profile +
//! environment + flags). `set` and `reset` rewrite the **file** so that the
//! layers above it are never baked into disk.

use std::path::{Path, PathBuf};

use broza::BrozaError;
use broza::config::{Config, keys, to_toml};
use broza::model::{Envelope, Host};

use crate::args::ConfigCommand;
use crate::commands::atomic;
use crate::output::{Renderer, envelope_to_json};

/// Everything `config` needs from `run()`.
#[derive(Debug, Clone)]
pub struct ConfigContext {
    /// Configuration file in use, existing or not.
    pub path: PathBuf,
    /// Contents of that file, or the defaults when it is absent.
    pub file: Config,
    /// The file after profile, environment and flag layering.
    pub effective: Config,
    /// Host block of the envelope.
    pub host: Host,
    /// RFC 3339 UTC timestamp of this invocation.
    pub generated_at: String,
    /// Whether Broza may ask a `y/N` question.
    pub interactive: bool,
}

/// Result of a `config` invocation, ready to render.
#[derive(Debug, Clone)]
pub struct ConfigOutput {
    payload: Payload,
    host: Host,
    generated_at: String,
}

#[derive(Debug, Clone)]
enum Payload {
    /// One key's value.
    Value { key: String, value: String },
    /// Every key with its value.
    Entries(Vec<(String, String)>),
    /// The configuration file path.
    Path(PathBuf),
    /// A confirmation line after a write.
    Written(String),
}

/// Execute `command` against `context`.
///
/// # Errors
///
/// [`BrozaError::Usage`] for unknown keys or invalid values,
/// [`BrozaError::Config`] when the file cannot be written, and
/// [`BrozaError::ConfirmationRequired`] when `reset` needs a confirmation it
/// cannot ask for.
pub fn run(command: &ConfigCommand, context: &ConfigContext) -> Result<ConfigOutput, BrozaError> {
    let payload = match command {
        ConfigCommand::Get { key } => {
            Payload::Value { key: key.clone(), value: keys::get(&context.effective, key)? }
        }
        ConfigCommand::List => Payload::Entries(keys::list(&context.effective)),
        ConfigCommand::Path => Payload::Path(context.path.clone()),
        ConfigCommand::Set { key, value } => {
            let updated = keys::set(context.file.clone(), key, value)?;
            write_config(&context.path, &updated)?;
            Payload::Written(format!("{key} = {value}"))
        }
        ConfigCommand::Reset { key } => reset(context, key.as_deref())?,
    };
    Ok(ConfigOutput { payload, host: context.host.clone(), generated_at: context.generated_at.clone() })
}

/// `reset [<key>]`. Resetting everything needs a confirmation (§3.6).
fn reset(context: &ConfigContext, key: Option<&str>) -> Result<Payload, BrozaError> {
    if key.is_none() && !context.interactive {
        return Err(BrozaError::ConfirmationRequired);
    }
    let updated = keys::reset(context.file.clone(), key)?;
    if key.is_none() && !confirm_reset_all(&context.path)? {
        return Err(BrozaError::AbortedByUser);
    }
    write_config(&context.path, &updated)?;
    Ok(Payload::Written(key.map_or_else(
        || "every key restored to its default".to_owned(),
        |name| format!("{name} restored to its default"),
    )))
}

/// Ask on stderr, read the answer from stdin. Only reached interactively.
fn confirm_reset_all(path: &Path) -> Result<bool, BrozaError> {
    use std::io::{BufRead, Write};

    let mut stderr = std::io::stderr();
    write!(stderr, "Restore every key in {} to its default? [y/N] ", path.display())
        .map_err(|source| BrozaError::Io { context: "asking for confirmation".to_owned(), source })?;
    stderr
        .flush()
        .map_err(|source| BrozaError::Io { context: "asking for confirmation".to_owned(), source })?;

    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|source| BrozaError::Io { context: "reading the answer".to_owned(), source })?;
    Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

/// Serialize and write `config` atomically (temp file + rename).
fn write_config(path: &Path, config: &Config) -> Result<(), BrozaError> {
    atomic::write(path, &to_toml(config)?)
}

impl Renderer for ConfigOutput {
    fn to_human(&self) -> String {
        match &self.payload {
            Payload::Value { value, .. } => value.clone(),
            Payload::Entries(entries) => {
                entries.iter().map(|(key, value)| format!("{key} = {value}")).collect::<Vec<_>>().join("\n")
            }
            Payload::Path(path) => path.display().to_string(),
            Payload::Written(message) => message.clone(),
        }
    }

    fn to_json(&self) -> Result<String, BrozaError> {
        let data = match &self.payload {
            Payload::Value { key, value } => serde_json::json!({ key: value }),
            Payload::Entries(entries) => serde_json::Value::Object(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
                    .collect(),
            ),
            Payload::Path(path) => serde_json::json!({ "path": path }),
            Payload::Written(message) => serde_json::json!({ "message": message }),
        };
        envelope_to_json(&Envelope::new("config", self.host.clone(), self.generated_at.clone(), data))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn context(dir: &Path) -> ConfigContext {
        ConfigContext {
            path: dir.join("config.toml"),
            file: Config::default(),
            effective: Config::default(),
            host: Host { macos_version: "26.1".into(), arch: "arm64".into() },
            generated_at: "2026-09-21T10:36:08Z".to_owned(),
            interactive: false,
        }
    }

    #[test]
    fn get_reports_the_effective_value() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ConfigContext {
            effective: Config { min_size: "2GB".into(), ..Config::default() },
            ..context(dir.path())
        };
        let out = run(&ConfigCommand::Get { key: "min-size".into() }, &ctx).unwrap();
        assert_eq!(out.to_human(), "2GB");
    }

    #[test]
    fn list_prints_every_key() {
        let dir = tempfile::tempdir().unwrap();
        let text = run(&ConfigCommand::List, &context(dir.path())).unwrap().to_human();
        assert_eq!(text.lines().count(), 8, "{text}");
        assert!(text.contains("unused-after = 1y"), "{text}");
    }

    #[test]
    fn path_prints_the_resolved_file() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let out = run(&ConfigCommand::Path, &ctx).unwrap();
        assert_eq!(out.to_human(), ctx.path.display().to_string());
    }

    #[test]
    fn set_writes_the_file_and_can_be_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        run(&ConfigCommand::Set { key: "min-size".into(), value: "2GB".into() }, &ctx).unwrap();

        let written = std::fs::read_to_string(&ctx.path).unwrap();
        assert!(written.contains("min-size = \"2GB\""), "{written}");
        assert!(!dir.path().join("config.toml.tmp").exists(), "temp file must be renamed away");
    }

    #[test]
    fn set_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ConfigContext { path: dir.path().join("nested/deeper/config.toml"), ..context(dir.path()) };
        run(&ConfigCommand::Set { key: "color".into(), value: "never".into() }, &ctx).unwrap();
        assert!(ctx.path.exists());
    }

    #[test]
    fn set_rejects_invalid_values_before_touching_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let err = run(&ConfigCommand::Set { key: "min-size".into(), value: "huge".into() }, &ctx)
            .expect_err("must fail");
        assert!(matches!(err, BrozaError::Usage(_)), "{err}");
        assert!(!ctx.path.exists(), "nothing must be written");
    }

    #[test]
    fn reset_of_one_key_needs_no_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ConfigContext {
            file: Config { min_size: "2GB".into(), ..Config::default() },
            ..context(dir.path())
        };
        run(&ConfigCommand::Reset { key: Some("min-size".into()) }, &ctx).unwrap();
        let written = std::fs::read_to_string(&ctx.path).unwrap();
        assert!(written.contains("min-size = \"50MB\""), "{written}");
    }

    #[test]
    fn reset_of_everything_without_a_tty_exits_seven() {
        let dir = tempfile::tempdir().unwrap();
        let err = run(&ConfigCommand::Reset { key: None }, &context(dir.path())).expect_err("must fail");
        assert_eq!(broza::ExitCode::from(&err), broza::ExitCode::ConfirmationRequired);
    }

    #[test]
    fn reset_rejects_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let err = run(&ConfigCommand::Reset { key: Some("nope".into()) }, &context(dir.path()))
            .expect_err("must fail");
        assert!(matches!(err, BrozaError::Usage(_)), "{err}");
    }

    #[test]
    fn list_json_uses_the_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let rendered = run(&ConfigCommand::List, &context(dir.path())).unwrap().to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["command"], "config");
        assert_eq!(parsed["schema_version"], broza::SCHEMA_VERSION);
        assert_eq!(parsed["data"]["min-size"], "50MB");
    }

    #[test]
    fn get_json_reports_the_single_key() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(&ConfigCommand::Get { key: "color".into() }, &context(dir.path())).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out.to_json().unwrap()).unwrap();
        assert_eq!(parsed["data"]["color"], "auto");
    }
}
