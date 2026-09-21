//! Snapshot of the process environment, taken once in [`crate::run`].
//!
//! Every later decision (color, interactivity, donation message, config path)
//! reads this struct instead of the real environment, so the whole CLI is
//! testable without touching the real `$HOME` or the real terminal.

use std::io::IsTerminal;
use std::path::{Component, Path, PathBuf};

use broza::config::EnvSnapshot;

/// Test and debug hook: `BROZA_HOST=<macos_version>/<arch>` pins the reported
/// host so integration tests never run `sw_vers`. Honoured only in debug
/// builds; release binaries always ask the system.
#[cfg(debug_assertions)]
const HOST_OVERRIDE_VAR: &str = "BROZA_HOST";

/// Test and debug hook: `BROZA_FAKE_DISKUTIL_FIXTURES=<dir>` replaces the
/// process runner with one replaying the recorded `diskutil` and `tmutil`
/// plists in `<dir>`, so `scan` and `explain` can be tested end to end without
/// a disk (`AGENTS.md` §7). Honoured only in debug builds compiled with the
/// `fake-diskutil` feature; a release binary always talks to the real system.
#[cfg(all(debug_assertions, feature = "fake-diskutil"))]
const FAKE_DISKUTIL_VAR: &str = "BROZA_FAKE_DISKUTIL_FIXTURES";

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
    /// Working directory, used to resolve a relative `explain <PATH>`.
    pub cwd: Option<PathBuf>,
    /// See `HOST_OVERRIDE_VAR`. Always `None` in release builds.
    pub host_override: Option<String>,
    /// See `FAKE_DISKUTIL_VAR`. Always `None` in release builds.
    pub fake_diskutil_fixtures: Option<PathBuf>,
    /// `TMPDIR`: macOS points it inside the per-uid temporary directory, which
    /// is one of the roots Broza may write under (`docs/cli-spec.md` §3.4).
    pub tmpdir: Option<PathBuf>,
}

/// Where macOS keeps the per-uid temporary directories.
const UID_TEMP_PARENT: &str = "/private/var/folders";

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
            cwd: std::env::current_dir().ok(),
            host_override: host_override(),
            fake_diskutil_fixtures: fake_diskutil_fixtures(),
            tmpdir: std::env::var_os("TMPDIR").map(PathBuf::from),
        }
    }

    /// The per-uid temporary directories (`/private/var/folders/<xx>/<hash>`)
    /// this process may clean under, derived from `TMPDIR`.
    ///
    /// macOS sets `TMPDIR` to `/var/folders/<xx>/<hash>/T/`; `/var` is a symlink
    /// to `/private/var`, and the guard wants the canonical spelling. Anything
    /// that does not have that shape yields no root at all rather than a guess.
    pub fn uid_temp_dirs(&self) -> Vec<PathBuf> {
        let Some(tmpdir) = self.tmpdir.as_deref() else { return Vec::new() };
        let canonical = match tmpdir.strip_prefix("/var") {
            Ok(rest) => Path::new("/private/var").join(rest),
            Err(_) => tmpdir.to_path_buf(),
        };
        let Ok(rest) = canonical.strip_prefix(UID_TEMP_PARENT) else { return Vec::new() };
        // A `.` or `..` in the two components that name the root is a shape
        // Broza does not name a root for, rather than something to normalise away.
        let mut parts = rest.components().map(|c| match c {
            Component::Normal(name) => Some(name),
            _ => None,
        });
        match (parts.next(), parts.next()) {
            (Some(Some(bucket)), Some(Some(hash))) => {
                vec![Path::new(UID_TEMP_PARENT).join(bucket).join(hash)]
            }
            _ => Vec::new(),
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
            cwd: None,
            host_override: None,
            fake_diskutil_fixtures: None,
            tmpdir: None,
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

#[cfg(all(debug_assertions, feature = "fake-diskutil"))]
fn fake_diskutil_fixtures() -> Option<PathBuf> {
    std::env::var_os(FAKE_DISKUTIL_VAR).map(PathBuf::from).filter(|dir| !dir.as_os_str().is_empty())
}

/// Release builds, and builds without the feature, always read the real disks.
#[cfg(not(all(debug_assertions, feature = "fake-diskutil")))]
const fn fake_diskutil_fixtures() -> Option<PathBuf> {
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
    #[test]
    fn the_uid_temp_dir_is_derived_from_tmpdir_in_its_canonical_spelling() {
        let env = RuntimeEnv {
            tmpdir: Some(PathBuf::from("/var/folders/zz/zyxvpxvq6csfxvn_n0000000000000/T/")),
            ..RuntimeEnv::for_tests(PathBuf::from("/Users/dana"))
        };

        assert_eq!(
            env.uid_temp_dirs(),
            vec![PathBuf::from("/private/var/folders/zz/zyxvpxvq6csfxvn_n0000000000000")]
        );
    }

    #[test]
    fn a_tmpdir_of_another_shape_yields_no_temporary_root() {
        for raw in ["/tmp", "/private/var/folders/zz", "relative/T", "/var/folders/../../Users/x/y/T"] {
            let env = RuntimeEnv {
                tmpdir: Some(PathBuf::from(raw)),
                ..RuntimeEnv::for_tests(PathBuf::from("/Users/d"))
            };
            assert!(env.uid_temp_dirs().is_empty(), "{raw}");
        }
        assert!(RuntimeEnv::for_tests(PathBuf::from("/Users/d")).uid_temp_dirs().is_empty());
    }
}
