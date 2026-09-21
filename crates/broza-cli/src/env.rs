//! Snapshot of the process environment, taken once in [`crate::run`].
//!
//! Every later decision (color, interactivity, donation message, config path)
//! reads this struct instead of the real environment, so the whole CLI is
//! testable without touching the real `$HOME` or the real terminal.

use std::io::IsTerminal;
use std::path::PathBuf;

use broza::config::EnvSnapshot;

/// Test and debug hook: `BROZA_HOST=<macos_version>/<arch>` pins the reported
/// host so integration tests never run `sw_vers`. Honoured only in debug
/// builds; release binaries always ask the system.
#[cfg(debug_assertions)]
const HOST_OVERRIDE_VAR: &str = "BROZA_HOST";

/// Everything the CLI is allowed to learn from the outside world.
// A snapshot of independent environment facts; grouping them would only add indirection.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEnv {
    /// stdout is an interactive terminal.
    pub stdout_is_tty: bool,
    /// stderr is an interactive terminal.
    pub stderr_is_tty: bool,
    /// stdin is an interactive terminal.
    pub stdin_is_tty: bool,
    /// `CI` is set: no prompts, no progress, no donation message.
    pub ci: bool,
    /// `NO_COLOR` is set.
    pub no_color: bool,
    /// `BROZA_NO_DONATE` is set.
    pub broza_no_donate: bool,
    /// `HOME`, or `None` when it is unset. Operations that need it fail with a
    /// clear usage error instead of writing to a guessed location.
    pub home: Option<PathBuf>,
    /// `BROZA_CONFIG`, lower priority than `--config`.
    pub broza_config: Option<PathBuf>,
    /// See `HOST_OVERRIDE_VAR`. Always `None` in release builds.
    pub host_override: Option<String>,
}

impl RuntimeEnv {
    /// Take a snapshot of the real process environment.
    pub fn from_process() -> Self {
        Self {
            stdout_is_tty: std::io::stdout().is_terminal(),
            stderr_is_tty: std::io::stderr().is_terminal(),
            stdin_is_tty: std::io::stdin().is_terminal(),
            ci: is_set("CI"),
            no_color: is_set("NO_COLOR"),
            broza_no_donate: is_set("BROZA_NO_DONATE"),
            home: std::env::var_os("HOME").map(PathBuf::from).filter(|home| !home.as_os_str().is_empty()),
            broza_config: std::env::var_os("BROZA_CONFIG").map(PathBuf::from),
            host_override: host_override(),
        }
    }

    /// A non-interactive snapshot rooted at `home`, for tests.
    pub fn for_tests(home: PathBuf) -> Self {
        Self {
            stdout_is_tty: false,
            stderr_is_tty: false,
            stdin_is_tty: false,
            ci: false,
            no_color: false,
            broza_no_donate: false,
            home: Some(home),
            broza_config: None,
            host_override: None,
        }
    }

    /// `true` when Broza may prompt: a TTY on stdin and stderr, and no `CI`.
    pub const fn is_interactive(&self) -> bool {
        self.stdin_is_tty && self.stderr_is_tty && !self.ci
    }

    /// The subset the core is allowed to see.
    pub fn to_core_snapshot(&self) -> EnvSnapshot {
        EnvSnapshot::default()
            .with_home(self.home.clone())
            .with_no_color(self.no_color)
            .with_broza_no_donate(self.broza_no_donate)
            .with_ci(self.ci)
            .with_broza_config(self.broza_config.clone())
    }
}

#[cfg(debug_assertions)]
fn host_override() -> Option<String> {
    std::env::var(HOST_OVERRIDE_VAR).ok().filter(|value| !value.is_empty())
}

/// Release builds ignore the override entirely.
#[cfg(not(debug_assertions))]
const fn host_override() -> Option<String> {
    None
}

/// A variable counts as "set" when present, whatever its value (`NO_COLOR` rule).
fn is_set(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn test_snapshot_is_non_interactive_by_default() {
        let env = RuntimeEnv::for_tests(PathBuf::from("/Users/test"));
        assert!(!env.is_interactive());
    }

    #[test]
    fn interactivity_needs_stdin_stderr_and_no_ci() {
        let base = RuntimeEnv {
            stdin_is_tty: true,
            stderr_is_tty: true,
            ..RuntimeEnv::for_tests(PathBuf::from("/Users/test"))
        };
        assert!(base.is_interactive());
        assert!(!RuntimeEnv { ci: true, ..base.clone() }.is_interactive());
        assert!(!RuntimeEnv { stdin_is_tty: false, ..base.clone() }.is_interactive());
        assert!(!RuntimeEnv { stderr_is_tty: false, ..base }.is_interactive());
    }

    #[test]
    fn a_process_snapshot_is_self_consistent() {
        // Reads the environment, never the filesystem: safe in a test.
        let env = RuntimeEnv::from_process();
        assert_eq!(env.is_interactive(), env.stdin_is_tty && env.stderr_is_tty && !env.ci);
        assert_eq!(env.to_core_snapshot().home, env.home);
    }

    #[test]
    fn core_snapshot_carries_only_what_the_core_may_see() {
        let env = RuntimeEnv {
            no_color: true,
            broza_no_donate: true,
            ci: true,
            broza_config: Some(PathBuf::from("/tmp/c.toml")),
            ..RuntimeEnv::for_tests(PathBuf::from("/Users/test"))
        };
        let snapshot = env.to_core_snapshot();
        assert_eq!(snapshot.home, Some(PathBuf::from("/Users/test")));
        assert!(snapshot.no_color && snapshot.broza_no_donate && snapshot.ci);
        assert_eq!(snapshot.broza_config, Some(PathBuf::from("/tmp/c.toml")));
    }

    #[test]
    fn an_unset_home_propagates_as_none_instead_of_a_guessed_path() {
        let env = RuntimeEnv { home: None, ..RuntimeEnv::for_tests(PathBuf::from("/Users/test")) };
        assert_eq!(env.to_core_snapshot().home, None);
    }
}
