//! The warnings a scan attaches to a volume: what was refused, what the cache
//! is filed under, what the file report could not carry, and what does not
//! add up.

use crate::model::{Diagnostic, Volume};
use crate::scan::walker::{self, DirNode, WalkResult};
use crate::scan::{MountEntry, ScanRequest, cache};

/// Warning code when the file report kept only the biggest files.
pub const FILE_REPORT_TRUNCATED_CODE: &str = "file_report_truncated";

/// The file report is a bounded set; when a home has more files above the
/// threshold than it keeps, the detectors reading it saw only the biggest.
/// `scan --top` is bounded on purpose and gets no warning.
pub(super) fn file_report_warning(
    walked: &WalkResult,
    request: &ScanRequest,
    entry: &MountEntry,
) -> Option<Diagnostic> {
    if request.file_report.is_none() || !walked.files_truncated {
        return None;
    }
    Some(Diagnostic {
        code: FILE_REPORT_TRUNCATED_CODE.to_owned(),
        message: format!(
            "{} holds more files above the reporting threshold than the {} kept; duplicates and large \
             old files were looked for among the biggest ones only",
            entry.volume.name,
            request.file_report().top
        ),
        path: Some(entry.mount_point.clone()),
    })
}

/// The warning a volume with no UUID earns, when a cache is in use at all.
///
/// A BSD name belongs to a slot, not to a disk: the next volume mounted there
/// would read this one's cache until it expires. Broza says so rather than
/// pretending the key is sound.
pub(super) fn cache_key_warning(request: &ScanRequest, volume: &Volume) -> Option<Diagnostic> {
    if request.cache_root.is_none() || volume.uuid.is_some() {
        return None;
    }
    let message = format!(
        "macOS reported no UUID for {}, so its scan cache is filed under the BSD name {}",
        volume.name, volume.id
    );
    Some(Diagnostic { code: cache::BSD_ID_KEY_CODE.to_owned(), message, path: volume.mount_point.clone() })
}

/// How many refused paths a collapsed warning still names.
const PERMISSION_EXAMPLES: usize = 5;
/// Stable code of the one warning that stands in for many refused paths.
pub const PERMISSION_SUMMARY_CODE: &str = "permission_denied_summary";

/// Fold a flood of `permission_denied` warnings into one that counts them.
///
/// A Mac without Full Disk Access refuses hundreds of paths in one scan; one
/// line per path buries every other warning. The summary keeps the first few
/// paths as examples; `verbose` keeps them all.
pub(super) fn collapse_permission_warnings(warnings: Vec<Diagnostic>, verbose: bool) -> Vec<Diagnostic> {
    let refused = warnings.iter().filter(|w| w.code == walker::PERMISSION_DENIED_CODE).count();
    if verbose || refused <= PERMISSION_EXAMPLES {
        return warnings;
    }
    let examples: Vec<String> = warnings
        .iter()
        .filter(|w| w.code == walker::PERMISSION_DENIED_CODE)
        .take(PERMISSION_EXAMPLES)
        .filter_map(|w| w.path.as_ref().map(|p| p.display().to_string()))
        .collect();
    let summary = Diagnostic {
        code: PERMISSION_SUMMARY_CODE.to_owned(),
        message: format!(
            "{refused} locations could not be read (for example {}); their sizes are missing from \
             the totals. Grant Full Disk Access to include them, or pass -v to list every path.",
            examples.join(", ")
        ),
        path: None,
    };
    std::iter::once(summary)
        .chain(warnings.into_iter().filter(|w| w.code != walker::PERMISSION_DENIED_CODE))
        .collect()
}

/// Stable code of the warning raised when a walk measures more than the volume holds.
pub const OVERCOUNT_CODE: &str = "size_exceeds_volume";

/// The warning a whole-volume walk earns when its total exceeds what macOS says
/// is in use.
///
/// Allocated blocks are summed per file. Hard links and APFS clone families are
/// counted once where the walk can see them whole, but a family split across a
/// subtree served from the cache, or one whose clones have diverged, still adds
/// up to more than the disk holds. Broza says so rather than letting the list
/// imply more space is freeable than exists (`AGENTS.md` §2.7, PRD RF-02).
pub(super) fn overcount_warning(root: &DirNode, entry: &MountEntry) -> Option<Diagnostic> {
    let used = entry.volume.used_bytes;
    if root.path != entry.mount_point || used == 0 || root.allocated_bytes <= used {
        return None;
    }
    Some(Diagnostic {
        code: OVERCOUNT_CODE.to_owned(),
        message: format!(
            "{} measures {} bytes but macOS reports {} in use on the volume. Clones and hard links \
             are counted once where the walk sees every copy, but not across a subtree served \
             from the cache, so the sizes listed are upper bounds; the volume figure also \
             includes snapshots and metadata a walk never sees.",
            entry.volume.name, root.allocated_bytes, used
        ),
        path: Some(entry.mount_point.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::FileReport;
    use crate::testing::mac_mount_table;

    #[test]
    fn a_truncated_file_report_warns_for_detectors_and_not_for_scan_top() {
        let table = mac_mount_table();
        let entry =
            table.volume_for(std::path::Path::new("/System/Volumes/Data")).unwrap_or_else(|| panic!("data"));
        let walked = WalkResult { files_truncated: true, ..WalkResult::default() };
        let for_detectors =
            ScanRequest { file_report: Some(FileReport::for_detectors()), ..ScanRequest::default() };

        let warned = file_report_warning(&walked, &for_detectors, entry);
        let silent = file_report_warning(&walked, &ScanRequest::default(), entry);
        let complete = file_report_warning(&WalkResult::default(), &for_detectors, entry);

        assert_eq!(warned.map(|w| w.code), Some(FILE_REPORT_TRUNCATED_CODE.to_owned()));
        assert!(silent.is_none() && complete.is_none());
    }
}
