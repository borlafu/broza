//! Where stdout data goes: the terminal, or the file given to `--output`.
//!
//! Principle 5 of `docs/cli-spec.md` §0: stdout is data. Prompts, progress and
//! warnings never come through here.

use std::io::Write;
use std::path::{Path, PathBuf};

use broza::BrozaError;

/// Destination of the command's data output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sink {
    /// The process's stdout.
    Stdout,
    /// A file, created or truncated on first write.
    File(PathBuf),
}

impl Sink {
    /// Choose the destination from `--output`.
    pub fn resolve(output: Option<&Path>) -> Self {
        output.map_or(Self::Stdout, |path| Self::File(path.to_path_buf()))
    }

    /// Write `payload`, appending a trailing newline when one is missing.
    ///
    /// # Errors
    ///
    /// [`BrozaError::Io`] when the file cannot be created or written, which the
    /// caller maps to exit code `1`.
    pub fn write(&self, payload: &str) -> Result<(), BrozaError> {
        let body = ensure_trailing_newline(payload);
        match self {
            Self::Stdout => std::io::stdout()
                .write_all(body.as_bytes())
                .map_err(|source| io_error("writing to stdout", source)),
            Self::File(path) => std::fs::write(path, body.as_bytes())
                .map_err(|source| io_error(&format!("writing to {}", path.display()), source)),
        }
    }
}

fn ensure_trailing_newline(payload: &str) -> String {
    if payload.ends_with('\n') { payload.to_owned() } else { format!("{payload}\n") }
}

fn io_error(context: &str, source: std::io::Error) -> BrozaError {
    BrozaError::Io { context: context.to_owned(), source }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn without_the_flag_data_goes_to_stdout() {
        assert_eq!(Sink::resolve(None), Sink::Stdout);
    }

    #[test]
    fn the_flag_selects_a_file() {
        assert_eq!(Sink::resolve(Some(Path::new("/tmp/a.json"))), Sink::File(PathBuf::from("/tmp/a.json")));
    }

    #[test]
    fn writing_creates_and_truncates_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let sink = Sink::File(path.clone());

        sink.write("first and much longer").unwrap();
        sink.write("second").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\n");
    }

    #[test]
    fn an_existing_trailing_newline_is_not_duplicated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        Sink::File(path.clone()).write("line\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "line\n");
    }

    #[test]
    fn an_unwritable_path_is_reported_with_context() {
        let sink = Sink::File(PathBuf::from("/nonexistent-broza-dir/out.txt"));
        let err = sink.write("x").expect_err("must fail");
        assert!(err.to_string().contains("/nonexistent-broza-dir/out.txt"), "{err}");
        assert_eq!(broza::ExitCode::from(&err), broza::ExitCode::GenericError);
    }
}
