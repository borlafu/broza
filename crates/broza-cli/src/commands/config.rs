//! `broza config get|set|list|path|reset` (`docs/cli-spec.md` §3.6).
//!
//! `get` and `list` report the **effective** configuration (file + profile +
//! environment + flags). `set` and `reset` rewrite the **file**, patching only
//! the keys they touch with `toml_edit` so comments, ordering and profiles all
//! survive.

use std::path::{Path, PathBuf};

use broza::BrozaError;
use broza::config::{Config, ConfigValue, keys};
use broza::model::{Envelope, Host, Warning};
use toml_edit::{Array, DocumentMut, Item, Value};

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
    /// Warnings collected before the command ran.
    pub warnings: Vec<Warning>,
}

/// Result of a `config` invocation, ready to render.
#[derive(Debug, Clone)]
pub struct ConfigOutput {
    payload: Payload,
    host: Host,
    generated_at: String,
    warnings: Vec<Warning>,
}

#[derive(Debug, Clone)]
enum Payload {
    /// One key's value, with its type preserved.
    Value { key: String, value: ConfigValue },
    /// Every key with its value.
    Entries(Vec<(String, ConfigValue)>),
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
/// [`BrozaError::PermissionDenied`] or [`BrozaError::Io`] when the file cannot
/// be written, and [`BrozaError::ConfirmationRequired`] when `reset` needs a
/// confirmation it cannot ask for.
pub fn run(command: &ConfigCommand, context: &ConfigContext) -> Result<ConfigOutput, BrozaError> {
    let payload = match command {
        ConfigCommand::Get { key } => {
            Payload::Value { key: key.clone(), value: keys::get(&context.effective, key)? }
        }
        ConfigCommand::List => Payload::Entries(keys::list(&context.effective)?),
        ConfigCommand::Path => Payload::Path(context.path.clone()),
        ConfigCommand::Set { key, values } => set(context, key, values)?,
        ConfigCommand::Reset { key, yes } => reset(context, key.as_deref(), *yes)?,
    };
    Ok(ConfigOutput {
        payload,
        host: context.host.clone(),
        generated_at: context.generated_at.clone(),
        warnings: context.warnings.clone(),
    })
}

/// `set <key> <value>...`: validate, then patch that one key in the document.
fn set(context: &ConfigContext, key: &str, values: &[String]) -> Result<Payload, BrozaError> {
    let updated = keys::set(context.file.clone(), key, values)?;
    let value = keys::get(&updated, key)?;
    patch(&context.path, &[(key.to_owned(), value.clone())])?;
    Ok(Payload::Written(format!("{key} = {}", value.to_human().replace('\n', ", "))))
}

/// `reset [<key>]`. Resetting everything needs a confirmation (§3.6).
fn reset(context: &ConfigContext, key: Option<&str>, yes: bool) -> Result<Payload, BrozaError> {
    let updated = keys::reset(context.file.clone(), key)?;
    let Some(key) = key else {
        confirm_reset_all(context, yes)?;
        patch(&context.path, &keys::list(&updated)?)?;
        return Ok(Payload::Written("every key restored to its default".to_owned()));
    };
    patch(&context.path, &[(key.to_owned(), keys::get(&updated, key)?)])?;
    Ok(Payload::Written(format!("{key} restored to its default")))
}

/// Resetting every key is destructive enough to need a `y/N` or `--yes`.
fn confirm_reset_all(context: &ConfigContext, yes: bool) -> Result<(), BrozaError> {
    if yes {
        return Ok(());
    }
    if !context.interactive {
        return Err(BrozaError::ConfirmationRequired);
    }
    if ask(&context.path)? { Ok(()) } else { Err(BrozaError::AbortedByUser) }
}

/// Ask on stderr, read the answer from stdin. Only reached interactively.
fn ask(path: &Path) -> Result<bool, BrozaError> {
    use std::io::{BufRead, Write};

    let mut stderr = std::io::stderr();
    let context = "asking for confirmation";
    write!(stderr, "Restore every key in {} to its default? [y/N] ", path.display())
        .and_then(|()| stderr.flush())
        .map_err(|source| BrozaError::Io { context: context.to_owned(), source })?;

    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|source| BrozaError::Io { context: "reading the answer".to_owned(), source })?;
    Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

/// Rewrite `path` with `entries` applied, leaving every other line untouched.
fn patch(path: &Path, entries: &[(String, ConfigValue)]) -> Result<(), BrozaError> {
    let mut document = read_document(path)?;
    for (key, value) in entries {
        document[key.as_str()] = to_item(value);
    }
    atomic::write(path, &document.to_string())
}

/// Parse the existing file, or start from an empty document when there is none.
fn read_document(path: &Path) -> Result<DocumentMut, BrozaError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(BrozaError::from_io("cannot read the configuration file", path, source));
        }
    };
    raw.parse::<DocumentMut>()
        .map_err(|source| BrozaError::Config(format!("invalid {}: {source}", path.display())))
}

fn to_item(value: &ConfigValue) -> Item {
    match value {
        ConfigValue::Text(text) => Item::Value(Value::from(text.as_str())),
        ConfigValue::Flag(flag) => Item::Value(Value::from(*flag)),
        ConfigValue::List(items) => {
            Item::Value(Value::Array(items.iter().map(String::as_str).collect::<Array>()))
        }
    }
}

impl Renderer for ConfigOutput {
    fn to_human(&self) -> String {
        match &self.payload {
            Payload::Value { value, .. } => value.to_human(),
            Payload::Entries(entries) => entries
                .iter()
                .map(|(key, value)| format!("{key} = {}", value.to_human().replace('\n', ", ")))
                .collect::<Vec<_>>()
                .join("\n"),
            Payload::Path(path) => path.display().to_string(),
            Payload::Written(message) => message.clone(),
        }
    }

    fn to_json(&self) -> Result<String, BrozaError> {
        let data = match &self.payload {
            Payload::Value { key, value } => {
                serde_json::Value::Object([(key.clone(), value.to_json())].into_iter().collect())
            }
            Payload::Entries(entries) => serde_json::Value::Object(
                entries.iter().map(|(key, value)| (key.clone(), value.to_json())).collect(),
            ),
            Payload::Path(path) => serde_json::json!({ "path": path }),
            Payload::Written(message) => serde_json::json!({ "message": message }),
        };
        let envelope = Envelope::new("config", self.host.clone(), self.generated_at.clone(), data);
        let envelope = self.warnings.iter().cloned().fold(envelope, Envelope::with_warning);
        envelope_to_json(&envelope)
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
            warnings: Vec::new(),
        }
    }

    fn values(raw: &[&str]) -> Vec<String> {
        raw.iter().map(ToString::to_string).collect()
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
        run(&ConfigCommand::Set { key: "min-size".into(), values: values(&["2GB"]) }, &ctx).unwrap();

        let written = std::fs::read_to_string(&ctx.path).unwrap();
        assert!(written.contains("min-size = \"2GB\""), "{written}");
    }

    #[test]
    fn set_preserves_comments_and_untouched_keys() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        std::fs::write(
            &ctx.path,
            "# keep me\nunused-after = \"2y\"  # inline note\n\n[profiles.dev]\nmin-size = \"1GB\"\n",
        )
        .unwrap();

        run(&ConfigCommand::Set { key: "min-size".into(), values: values(&["2GB"]) }, &ctx).unwrap();

        let written = std::fs::read_to_string(&ctx.path).unwrap();
        assert!(written.contains("# keep me"), "{written}");
        assert!(written.contains("# inline note"), "{written}");
        assert!(written.contains("unused-after = \"2y\""), "{written}");
        assert!(written.contains("[profiles.dev]"), "{written}");
        assert!(written.contains("min-size = \"2GB\""), "{written}");
    }

    #[test]
    fn set_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ConfigContext { path: dir.path().join("nested/deeper/config.toml"), ..context(dir.path()) };
        run(&ConfigCommand::Set { key: "color".into(), values: values(&["never"]) }, &ctx).unwrap();
        assert!(ctx.path.exists());
    }

    #[test]
    fn set_rejects_invalid_values_before_touching_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let err = run(&ConfigCommand::Set { key: "min-size".into(), values: values(&["huge"]) }, &ctx)
            .expect_err("must fail");
        assert!(matches!(err, BrozaError::Usage(_)), "{err}");
        assert!(!ctx.path.exists(), "nothing must be written");
    }

    #[test]
    fn exclude_is_written_as_an_array_of_verbatim_globs() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let globs = values(&["~/p/**/*.{js,ts}", "~/Library/Caches/com.a,b.*"]);
        run(&ConfigCommand::Set { key: "exclude".into(), values: globs.clone() }, &ctx).unwrap();

        let written = std::fs::read_to_string(&ctx.path).unwrap();
        assert!(written.contains("~/p/**/*.{js,ts}"), "{written}");
        let reparsed = broza::config::parse(&written, &ctx.path).unwrap();
        assert_eq!(reparsed.exclude, globs);
    }

    #[test]
    fn reset_of_one_key_needs_no_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ConfigContext {
            file: Config { min_size: "2GB".into(), ..Config::default() },
            ..context(dir.path())
        };
        run(&ConfigCommand::Reset { key: Some("min-size".into()), yes: false }, &ctx).unwrap();
        let written = std::fs::read_to_string(&ctx.path).unwrap();
        assert!(written.contains("min-size = \"50MB\""), "{written}");
    }

    #[test]
    fn reset_of_everything_without_a_tty_exits_seven() {
        let dir = tempfile::tempdir().unwrap();
        let err = run(&ConfigCommand::Reset { key: None, yes: false }, &context(dir.path()))
            .expect_err("must fail");
        assert_eq!(broza::ExitCode::from(&err), broza::ExitCode::ConfirmationRequired);
        assert!(!dir.path().join("config.toml").exists(), "nothing must be written");
    }

    #[test]
    fn reset_of_everything_with_yes_skips_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ConfigContext {
            file: Config { min_size: "2GB".into(), ..Config::default() },
            ..context(dir.path())
        };
        std::fs::write(&ctx.path, "# survivor\nmin-size = \"2GB\"\n").unwrap();

        run(&ConfigCommand::Reset { key: None, yes: true }, &ctx).unwrap();

        let written = std::fs::read_to_string(&ctx.path).unwrap();
        assert!(written.contains("# survivor"), "comments survive a full reset: {written}");
        let reparsed = broza::config::parse(&written, &ctx.path).unwrap();
        assert_eq!(reparsed, Config::default());
    }

    #[test]
    fn reset_rejects_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let err = run(&ConfigCommand::Reset { key: Some("nope".into()), yes: true }, &context(dir.path()))
            .expect_err("must fail");
        assert!(matches!(err, BrozaError::Usage(_)), "{err}");
    }

    #[test]
    fn list_json_emits_typed_values() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ConfigContext {
            effective: Config { exclude: vec!["~/a/**".into()], ..Config::default() },
            ..context(dir.path())
        };
        let rendered = run(&ConfigCommand::List, &ctx).unwrap().to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["command"], "config");
        assert_eq!(parsed["data"]["min-size"], serde_json::json!("50MB"));
        assert_eq!(parsed["data"]["donate-prompt"], serde_json::json!(true));
        assert_eq!(parsed["data"]["exclude"], serde_json::json!(["~/a/**"]));
    }

    #[test]
    fn get_json_reports_the_single_key() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(&ConfigCommand::Get { key: "donate-prompt".into() }, &context(dir.path())).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out.to_json().unwrap()).unwrap();
        assert_eq!(parsed["data"]["donate-prompt"], serde_json::json!(true));
    }

    #[test]
    fn host_warnings_reach_the_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ConfigContext {
            warnings: vec![Warning {
                code: "host_version_unknown".into(),
                message: "no version".into(),
                path: None,
            }],
            ..context(dir.path())
        };
        let rendered = run(&ConfigCommand::List, &ctx).unwrap().to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["warnings"][0]["code"], "host_version_unknown");
    }
}
