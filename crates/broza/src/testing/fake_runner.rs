//! Scripted [`ProcessRunner`](crate::ports::ProcessRunner).
//!
//! A `FakeRunner` answers `(program, args)` with a recorded [`ProcessOutput`] or a
//! scripted failure and never spawns anything. Every invocation is recorded so a test
//! can assert on the exact command line Broza would have run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use crate::BrozaError;
use crate::ports::{ProcessOutput, ProcessRunner};
use crate::testing::sync::lock;

/// Directory holding the recorded command fixtures (`crates/broza/tests/fixtures`).
const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// One invocation seen by a [`FakeRunner`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCall {
    /// Program the caller asked for.
    pub program: String,
    /// Arguments the caller passed.
    pub args: Vec<String>,
    /// Timeout the caller asked for.
    pub timeout: Duration,
}

/// What a scripted command line answers with.
#[derive(Debug, Clone)]
enum Scripted {
    /// A finished process.
    Output(Box<ProcessOutput>),
    /// A failure to run the process at all.
    Failure(String),
}

/// A [`ProcessRunner`] that replays scripted answers instead of spawning processes.
///
/// ```
/// # use std::time::Duration;
/// # use broza::ports::{ProcessOutput, ProcessRunner};
/// # use broza::testing::FakeRunner;
/// let out = ProcessOutput { success: true, code: Some(0), stdout: b"ok".to_vec(), stderr: vec![] };
/// let runner = FakeRunner::new().with_output("diskutil", &["list"], out);
/// let result = runner.run("diskutil", &["list"], Duration::from_secs(1));
/// assert_eq!(result.map(|o| o.stdout_text()).unwrap_or_default(), "ok");
/// assert_eq!(runner.calls().len(), 1);
/// ```
#[derive(Debug, Default)]
pub struct FakeRunner {
    /// Scripted answers keyed by the rendered command line.
    scripted: Mutex<BTreeMap<String, Scripted>>,
    /// Every invocation, in order.
    calls: Mutex<Vec<RecordedCall>>,
}

impl FakeRunner {
    /// A runner with nothing scripted: every invocation fails.
    pub fn new() -> Self {
        Self::default()
    }

    /// Script `output` for `program` invoked with exactly `args`.
    pub fn script_output(&self, program: &str, args: &[&str], output: ProcessOutput) {
        lock(&self.scripted).insert(command_key(program, args), Scripted::Output(Box::new(output)));
    }

    /// Script a failure to run `program` with exactly `args`.
    pub fn script_failure(&self, program: &str, args: &[&str], message: &str) {
        lock(&self.scripted).insert(command_key(program, args), Scripted::Failure(message.to_owned()));
    }

    /// Builder form of [`FakeRunner::script_output`].
    #[must_use]
    pub fn with_output(self, program: &str, args: &[&str], output: ProcessOutput) -> Self {
        self.script_output(program, args, output);
        self
    }

    /// Builder form of [`FakeRunner::script_failure`].
    #[must_use]
    pub fn with_failure(self, program: &str, args: &[&str], message: &str) -> Self {
        self.script_failure(program, args, message);
        self
    }

    /// Script a successful run whose standard output is the fixture at
    /// `relative_path` inside `crates/broza/tests/fixtures/`.
    pub fn script_fixture(
        &self,
        program: &str,
        args: &[&str],
        relative_path: &str,
    ) -> Result<(), BrozaError> {
        let path = Self::fixture_path(relative_path);
        let stdout = std::fs::read(&path).map_err(|source| BrozaError::Io {
            context: format!("read fixture {}", path.display()),
            source,
        })?;
        self.script_output(
            program,
            args,
            ProcessOutput { success: true, code: Some(0), stdout, stderr: Vec::new() },
        );
        Ok(())
    }

    /// Builder form of [`FakeRunner::script_fixture`].
    pub fn with_fixture(self, program: &str, args: &[&str], relative_path: &str) -> Result<Self, BrozaError> {
        self.script_fixture(program, args, relative_path)?;
        Ok(self)
    }

    /// Absolute path of a fixture, for tests that need to read one directly.
    pub fn fixture_path(relative_path: &str) -> PathBuf {
        Path::new(FIXTURE_DIR).join(relative_path)
    }

    /// Snapshot of every invocation seen so far, in order.
    pub fn calls(&self) -> Vec<RecordedCall> {
        lock(&self.calls).clone()
    }

    /// Rendered command lines this runner can answer, for assertions and messages.
    pub fn expected_keys(&self) -> Vec<String> {
        lock(&self.scripted).keys().cloned().collect()
    }

    /// Error describing that `key` was not scripted.
    fn unmatched(&self, key: &str) -> BrozaError {
        let expected = self.expected_keys();
        let listed = if expected.is_empty() { "nothing".to_owned() } else { expected.join("`, `") };
        BrozaError::Other(format!("FakeRunner: no answer scripted for `{key}`; expected: `{listed}`"))
    }
}

impl ProcessRunner for FakeRunner {
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> Result<ProcessOutput, BrozaError> {
        let key = command_key(program, args);
        let call = RecordedCall {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            timeout,
        };
        lock(&self.calls).push(call);
        let scripted = lock(&self.scripted).get(&key).cloned();
        match scripted.as_ref() {
            Some(Scripted::Output(output)) => Ok((**output).clone()),
            Some(Scripted::Failure(message)) => Err(BrozaError::Other(message.clone())),
            None => Err(self.unmatched(&key)),
        }
    }
}

/// Render `(program, args)` as the key a scripted answer is stored under.
fn command_key(program: &str, args: &[&str]) -> String {
    if args.is_empty() { program.to_owned() } else { format!("{program} {}", args.join(" ")) }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::BrozaError;
    use crate::ports::{ProcessOutput, ProcessRunner};
    use crate::testing::FakeRunner;

    const TIMEOUT: Duration = Duration::from_secs(1);

    fn output(stdout: &str) -> ProcessOutput {
        ProcessOutput { success: true, code: Some(0), stdout: stdout.as_bytes().to_vec(), stderr: Vec::new() }
    }

    #[test]
    fn returns_the_output_scripted_for_the_program_and_arguments() {
        let runner = FakeRunner::new().with_output("diskutil", &["list", "-plist"], output("<plist/>"));

        let result = runner.run("diskutil", &["list", "-plist"], TIMEOUT);

        let out = result.unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(out.stdout_text(), "<plist/>");
    }

    #[test]
    fn records_every_invocation_in_order() {
        let runner =
            FakeRunner::new().with_output("a", &[], output("1")).with_output("b", &["x"], output("2"));

        let _ = runner.run("a", &[], TIMEOUT);
        let _ = runner.run("b", &["x"], TIMEOUT);

        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].program, "a");
        assert_eq!(calls[1].args, vec!["x".to_owned()]);
        assert_eq!(calls[1].timeout, TIMEOUT);
    }

    #[test]
    fn an_unmatched_invocation_lists_the_expected_keys() {
        let runner = FakeRunner::new().with_output("diskutil", &["list"], output(""));

        let err = runner.run("tmutil", &["listlocalsnapshots"], TIMEOUT).err();

        let Some(BrozaError::Other(message)) = err else { panic!("expected BrozaError::Other") };
        assert!(message.contains("tmutil listlocalsnapshots"), "{message}");
        assert!(message.contains("diskutil list"), "{message}");
    }

    #[test]
    fn an_unmatched_invocation_is_still_recorded() {
        let runner = FakeRunner::new();

        let _ = runner.run("ghost", &[], TIMEOUT);

        assert_eq!(runner.calls().len(), 1);
    }

    #[test]
    fn a_scripted_failure_is_returned_as_an_error() {
        let runner = FakeRunner::new().with_failure("diskutil", &["info"], "boom");

        let err = runner.run("diskutil", &["info"], TIMEOUT).err();

        assert!(matches!(err, Some(BrozaError::Other(message)) if message == "boom"));
    }

    #[test]
    fn a_fixture_becomes_the_standard_output_of_a_successful_run() {
        let runner = FakeRunner::new()
            .with_fixture("diskutil", &["list"], "json/4_1_envelope.json")
            .unwrap_or_else(|e| panic!("{e}"));

        let out = runner.run("diskutil", &["list"], TIMEOUT).unwrap_or_else(|e| panic!("{e}"));

        assert!(out.success);
        assert_eq!(out.code, Some(0));
        assert!(out.stdout_text().contains("schema_version"));
    }

    #[test]
    fn a_missing_fixture_fails_when_the_runner_is_built() {
        let result = FakeRunner::new().with_fixture("diskutil", &["list"], "nope/missing.plist");

        assert!(result.is_err());
    }
}
