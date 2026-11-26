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
    let Some(home) = home else { return };
    if no_cache || ports.fs.exists(&home.join(CACHE_DIR)) {
        return;
    }
    let _ignored = writeln!(std::io::stderr(), "{FIRST_SCAN_NOTE}");
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
}
