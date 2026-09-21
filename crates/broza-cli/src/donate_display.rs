//! Showing the donation message once the gate says yes (`docs/cli-spec.md` §5).
//!
//! [`crate::donate`] is the pure predicate; this module gathers its inputs from
//! the run (the outcome, the flags, the environment, the marker file), prints
//! the two lines on stderr and rewrites the marker. A marker that cannot be
//! written is mentioned at `-v` and never changes the exit code.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use broza::ports::{Clock, FileOps};
use jiff::Timestamp;

use crate::cli::GlobalArgs;
use crate::commands::{Outcome, Reclaimed};
use crate::donate::{DONATE_URL, DonationInput, MARKER_RELATIVE, should_show_donation};
use crate::env::RuntimeEnv;
use crate::output::{OutputFormat, format_bytes};

/// What a run has to have been for the message to be considered at all.
pub struct Run<'a> {
    /// The command's outcome, with its reclaim figures when it applied a cleanup.
    pub outcome: &'a Outcome,
    /// The global flags (`--json`, `--csv`, `--quiet`, `-v`).
    pub global: &'a GlobalArgs,
    /// The environment (TTYs, `CI`, `BROZA_NO_DONATE`, home).
    pub runtime: &'a RuntimeEnv,
    /// The `donate-prompt` configuration key.
    pub donate_prompt: bool,
    /// Format the caller asked for.
    pub format: OutputFormat,
}

/// Show the message when every condition holds, and remember having done so.
pub fn maybe_show(run: &Run<'_>, fs: &dyn FileOps, clock: &dyn Clock) {
    let Some(reclaimed) = run.outcome.reclaimed.as_ref() else { return };
    let Some(home) = run.runtime.home.as_deref() else { return };
    let marker = marker_path(home);
    let now = clock.now();
    let input = DonationInput {
        apply_succeeded: run.outcome.code == broza::ExitCode::Ok,
        affected_bytes: reclaimed.total(),
        stdout_is_tty: run.runtime.stdout_is_tty,
        stderr_is_tty: run.runtime.stderr_is_tty,
        json: run.format == OutputFormat::Json,
        csv: run.format == OutputFormat::Csv,
        quiet: run.global.quiet,
        donate_prompt: run.donate_prompt,
        broza_no_donate: run.runtime.broza_no_donate,
        ci: run.runtime.ci,
        last_shown: read_marker(fs, &marker),
        now,
    };
    if !should_show_donation(&input) {
        return;
    }
    let mut stderr = std::io::stderr();
    for line in message(reclaimed) {
        let _ignored = writeln!(stderr, "{line}");
    }
    if let Err(error) = write_marker(fs, &marker, now)
        && run.global.verbose > 0
    {
        let _ignored = writeln!(stderr, "warning: the donation marker could not be written: {error}");
    }
}

/// The two lines of §5, with a zero figure's parenthesis left out.
pub fn message(reclaimed: &Reclaimed) -> [String; 2] {
    let parts: Vec<String> =
        [(reclaimed.quarantined_bytes, "in quarantine"), (reclaimed.freed_bytes, "freed")]
            .into_iter()
            .filter(|(bytes, _)| *bytes > 0)
            .map(|(bytes, what)| format!("{} {what}", format_bytes(bytes)))
            .collect();
    let detail = if parts.len() == 2 { format!(" ({})", parts.join(", ")) } else { String::new() };
    [
        format!(
            "  Broza made {} reclaimable{detail}. It is free and open source software.",
            format_bytes(reclaimed.total())
        ),
        format!("  If it helped you: {DONATE_URL}   ·   Silence this: broza config set donate-prompt false"),
    ]
}

/// Where the marker lives for `home`.
pub fn marker_path(home: &Path) -> PathBuf {
    home.join(MARKER_RELATIVE)
}

/// The instant the message was last shown, or `None` for never or unreadable.
fn read_marker(fs: &dyn FileOps, marker: &Path) -> Option<Timestamp> {
    let raw = fs.read(marker).ok()?;
    String::from_utf8(raw).ok()?.trim().parse().ok()
}

/// Remember `now` as the last time the message was shown.
fn write_marker(fs: &dyn FileOps, marker: &Path, now: Timestamp) -> Result<(), broza::BrozaError> {
    if let Some(parent) = marker.parent() {
        fs.create_dir_all(parent)?;
    }
    fs.write_atomic(marker, now.to_string().as_bytes())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use broza::testing::{FakeFileOps, FixedClock};

    use super::*;

    fn reclaimed(quarantined: u64, freed: u64) -> Reclaimed {
        Reclaimed { quarantined_bytes: quarantined, freed_bytes: freed }
    }

    #[test]
    fn the_message_reports_pending_and_freed_bytes_separately_and_drops_a_zero() {
        let both = message(&reclaimed(121_400_000_000, 16_800_000_000));
        let pending_only = message(&reclaimed(94_200_000_000, 0));

        assert_eq!(
            both[0],
            "  Broza made 138.2 GB reclaimable (121.4 GB in quarantine, 16.8 GB freed). It is free and open source software."
        );
        assert_eq!(
            both[1],
            "  If it helped you: https://ko-fi.com/broza   ·   Silence this: broza config set donate-prompt false"
        );
        assert_eq!(pending_only[0], "  Broza made 94.2 GB reclaimable. It is free and open source software.");
    }

    fn interactive(home: &str) -> RuntimeEnv {
        RuntimeEnv {
            stdout_is_tty: true,
            stderr_is_tty: true,
            stdin_is_tty: true,
            ..RuntimeEnv::for_tests(PathBuf::from(home))
        }
    }

    fn global() -> GlobalArgs {
        GlobalArgs {
            json: false,
            csv: false,
            output: None,
            no_color: false,
            quiet: false,
            verbose: 0,
            config: None,
            profile: None,
            no_cache: false,
        }
    }

    #[test]
    fn an_eligible_run_writes_the_marker_and_a_second_run_within_the_cooldown_does_not_rewrite_it() {
        let fs = FakeFileOps::new().with_root("/", 1).with_dir("/Users/dana");
        let clock = FixedClock::at("2026-09-21T10:36:08Z".parse().unwrap());
        let outcome = Outcome::ok(String::new()).with_reclaimed(reclaimed(10, 0));
        let runtime = interactive("/Users/dana");
        let run = Run {
            outcome: &outcome,
            global: &global(),
            runtime: &runtime,
            donate_prompt: true,
            format: OutputFormat::Human,
        };

        maybe_show(&run, &fs, &clock);
        let marker = marker_path(Path::new("/Users/dana"));
        let first = fs.read(&marker).unwrap();
        clock.advance(std::time::Duration::from_secs(3600));
        maybe_show(&run, &fs, &clock);

        assert_eq!(String::from_utf8(first.clone()).unwrap(), "2026-09-21T10:36:08Z");
        assert_eq!(fs.read(&marker).unwrap(), first, "shown once, remembered once");
    }

    #[test]
    fn a_run_that_reclaimed_nothing_or_was_not_interactive_leaves_no_marker() {
        let fs = FakeFileOps::new().with_root("/", 1).with_dir("/Users/dana");
        let clock = FixedClock::at("2026-09-21T10:36:08Z".parse().unwrap());
        let dry = Outcome::ok(String::new());
        let quiet_env = RuntimeEnv::for_tests(PathBuf::from("/Users/dana"));
        let applied = Outcome::ok(String::new()).with_reclaimed(reclaimed(10, 0));
        let interactive_env = interactive("/Users/dana");

        maybe_show(
            &Run {
                outcome: &dry,
                global: &global(),
                runtime: &interactive_env,
                donate_prompt: true,
                format: OutputFormat::Human,
            },
            &fs,
            &clock,
        );
        maybe_show(
            &Run {
                outcome: &applied,
                global: &global(),
                runtime: &quiet_env,
                donate_prompt: true,
                format: OutputFormat::Human,
            },
            &fs,
            &clock,
        );
        maybe_show(
            &Run {
                outcome: &applied,
                global: &global(),
                runtime: &interactive_env,
                donate_prompt: true,
                format: OutputFormat::Json,
            },
            &fs,
            &clock,
        );

        assert!(!fs.exists(&marker_path(Path::new("/Users/dana"))));
    }
}
