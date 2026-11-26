//! The scan's stderr progress: one line rewritten in place while walking, and
//! a note before a scan that has no cache to start from (`docs/cli-spec.md` §3.1).

use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use broza::ports::Ports;
use broza::scan::ScanProgress;

use super::folders::CACHE_DIR;
use crate::output::format_bytes;

/// The note before a scan with no cache yet.
const FIRST_SCAN_NOTE: &str =
    "First scan: Broza reads every folder once and keeps a cache, so the scans after this one are faster.";

/// One progress line on stderr, rewritten in place.
pub fn print(progress: ScanProgress, elapsed: Duration) {
    let mut stderr = std::io::stderr();
    let _ignored = write!(stderr, "\r\x1b[2K{}", progress_line(progress, elapsed));
    let _ignored = stderr.flush();
}

/// The text of the progress line.
fn progress_line(progress: ScanProgress, elapsed: Duration) -> String {
    format!(
        "Scanning… {} entries, {} seen  ·  {} s",
        progress.entries_scanned,
        format_bytes(progress.bytes_scanned),
        elapsed.as_secs()
    )
}

/// Said once before a walk that has no cache to start from, so a long first
/// scan reads as expected rather than as slow.
pub fn announce_first_scan(home: Option<&Path>, no_cache: bool, ports: &Ports) {
    if let Some(note) = first_scan_note(home, no_cache, ports) {
        let _ignored = writeln!(std::io::stderr(), "{note}");
    }
}

/// The note, when there is a cache to build and none exists yet.
fn first_scan_note(home: Option<&Path>, no_cache: bool, ports: &Ports) -> Option<&'static str> {
    let home = home?;
    (!no_cache && !ports.fs.exists(&home.join(CACHE_DIR))).then_some(FIRST_SCAN_NOTE)
}

/// Wipe the progress line before the report is printed.
pub fn clear() {
    let mut stderr = std::io::stderr();
    let _ignored = write!(stderr, "\r\x1b[2K");
    let _ignored = stderr.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_progress_line_shows_entries_bytes_and_elapsed_seconds() {
        let progress = ScanProgress { entries_scanned: 1234, bytes_scanned: 5_000_000_000 };

        assert_eq!(
            progress_line(progress, Duration::from_millis(12_900)),
            "Scanning… 1234 entries, 5.0 GB seen  ·  12 s"
        );
    }

    #[test]
    fn the_first_scan_note_is_said_only_when_a_cache_is_wanted_and_missing() {
        let (ports, handles) = broza::testing::fake_ports();
        handles.fs.add_root("/", 1);
        handles.fs.add_dir("/Users/dana");
        let home = Some(Path::new("/Users/dana"));

        assert_eq!(first_scan_note(home, false, &ports), Some(FIRST_SCAN_NOTE));
        assert_eq!(first_scan_note(home, true, &ports), None, "--no-cache builds nothing");
        assert_eq!(first_scan_note(None, false, &ports), None, "no home, no cache");
        handles.fs.add_dir("/Users/dana/.cache/broza");
        assert_eq!(first_scan_note(home, false, &ports), None, "a cache exists");
    }
}
