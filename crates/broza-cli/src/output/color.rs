//! Color policy (`docs/cli-spec.md` §1.1, implementation plan §3.5).
//!
//! Color is disabled by `--no-color`, `NO_COLOR`, `--json`, `--csv`, a non-TTY
//! stdout, or `CI`. Anything left is decided by the `color` configuration key.

use broza::config::ColorChoice;

use crate::env::RuntimeEnv;
use crate::output::format::OutputFormat;

/// What the renderer is allowed to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorPolicy {
    /// No ANSI escapes at all.
    Never,
    /// ANSI escapes are allowed because stdout is an interactive terminal.
    Auto,
    /// ANSI escapes are forced by configuration.
    Always,
}

impl ColorPolicy {
    /// Resolve the policy from the flag, the configuration, the environment and
    /// the output format.
    pub fn resolve(
        flag_no_color: bool,
        config_color: ColorChoice,
        env: &RuntimeEnv,
        format: OutputFormat,
    ) -> Self {
        let forced_off = flag_no_color
            || env.no_color
            || env.ci
            || !env.stdout_is_tty
            || config_color == ColorChoice::Never
            || format.is_machine_readable();
        if forced_off {
            return Self::Never;
        }
        match config_color {
            ColorChoice::Always => Self::Always,
            _ => Self::Auto,
        }
    }

    /// Whether ANSI escapes may be written.
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Auto | Self::Always)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;

    use super::*;

    fn tty_env() -> RuntimeEnv {
        RuntimeEnv {
            stdout_is_tty: true,
            stderr_is_tty: true,
            stdin_is_tty: true,
            ..RuntimeEnv::for_tests(PathBuf::from("/Users/test"))
        }
    }

    #[test]
    fn interactive_terminal_with_auto_config_allows_color() {
        let policy = ColorPolicy::resolve(false, ColorChoice::Auto, &tty_env(), OutputFormat::Human);
        assert_eq!(policy, ColorPolicy::Auto);
        assert!(policy.is_enabled());
    }

    #[test]
    fn always_in_configuration_forces_color_on_a_terminal() {
        let policy = ColorPolicy::resolve(false, ColorChoice::Always, &tty_env(), OutputFormat::Human);
        assert_eq!(policy, ColorPolicy::Always);
    }

    #[test]
    fn every_documented_reason_disables_color() {
        let cases: Vec<(&str, bool, ColorChoice, RuntimeEnv, OutputFormat)> = vec![
            ("--no-color", true, ColorChoice::Auto, tty_env(), OutputFormat::Human),
            (
                "NO_COLOR",
                false,
                ColorChoice::Auto,
                RuntimeEnv { no_color: true, ..tty_env() },
                OutputFormat::Human,
            ),
            ("CI", false, ColorChoice::Auto, RuntimeEnv { ci: true, ..tty_env() }, OutputFormat::Human),
            (
                "non-TTY stdout",
                false,
                ColorChoice::Auto,
                RuntimeEnv { stdout_is_tty: false, ..tty_env() },
                OutputFormat::Human,
            ),
            ("--json", false, ColorChoice::Auto, tty_env(), OutputFormat::Json),
            ("--csv", false, ColorChoice::Auto, tty_env(), OutputFormat::Csv),
            ("color=never", false, ColorChoice::Never, tty_env(), OutputFormat::Human),
        ];
        for (reason, flag, config, env, format) in cases {
            assert_eq!(
                ColorPolicy::resolve(flag, config, &env, format),
                ColorPolicy::Never,
                "{reason} must disable color"
            );
        }
    }

    #[test]
    fn always_does_not_survive_a_machine_readable_format() {
        let policy = ColorPolicy::resolve(false, ColorChoice::Always, &tty_env(), OutputFormat::Json);
        assert_eq!(policy, ColorPolicy::Never);
        assert!(!policy.is_enabled());
    }
}
