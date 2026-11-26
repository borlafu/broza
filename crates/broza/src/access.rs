//! Whether this process has Full Disk Access (`docs/cli-spec.md` §6).
//!
//! macOS has no public call that answers the question, so Broza asks the way
//! every tool does: it lists a folder that only a program with Full Disk Access
//! may list. The folder is under the home, so the answer never depends on
//! ownership: a refusal there is the privacy control and nothing else.
//!
//! Root-owned system folders (`.Spotlight-V100`, `.fseventsd`) are refused
//! with or without Full Disk Access, which is why a scan's refusals alone
//! cannot tell the two cases apart and this check exists.

use std::path::Path;

use crate::BrozaError;
use crate::model::{AccessCheck, AccessState};
use crate::ports::FileOps;

/// Protected folders, relative to the home, in the order they are tried.
/// The first one is on every Mac; the others cover a home where it is not.
const PROTECTED: [&str; 4] =
    ["Library/Application Support/com.apple.TCC", "Library/Safari", "Library/Mail", "Library/Messages"];

/// List the first protected folder under `home` that exists and say what
/// macOS answered. Listing is the whole test: no `stat` first, so the answer
/// never depends on macOS letting a protected folder be stated.
pub fn check_full_disk_access(fs: &dyn FileOps, home: &Path) -> AccessCheck {
    for relative in PROTECTED {
        let probe = home.join(relative);
        let state = match fs.read_dir(&probe) {
            Ok(_) => AccessState::Granted,
            Err(BrozaError::PermissionDenied { .. }) => AccessState::Missing,
            Err(BrozaError::TargetNotFound(_)) => continue,
            Err(_) => AccessState::Unknown,
        };
        return AccessCheck { state, probe_path: Some(probe) };
    }
    AccessCheck { state: AccessState::Unknown, probe_path: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeFileOps;

    const HOME: &str = "/Users/dana";

    fn home_with(folder: &str) -> FakeFileOps {
        FakeFileOps::new().with_root("/", 1).with_dir(Path::new(HOME).join(folder))
    }

    #[test]
    fn a_protected_folder_that_lists_means_access_is_granted() {
        let fs = home_with(PROTECTED[0]);

        let check = check_full_disk_access(&fs, Path::new(HOME));

        assert_eq!(check.state, AccessState::Granted);
        assert_eq!(check.probe_path, Some(Path::new(HOME).join(PROTECTED[0])));
    }

    #[test]
    fn a_protected_folder_macos_refuses_means_access_is_missing() {
        let fs = home_with(PROTECTED[0]).with_denied(Path::new(HOME).join(PROTECTED[0]));

        assert_eq!(check_full_disk_access(&fs, Path::new(HOME)).state, AccessState::Missing);
    }

    #[test]
    fn the_next_protected_folder_is_tried_when_the_first_does_not_exist() {
        let fs = home_with("Library/Mail").with_denied(Path::new(HOME).join("Library/Mail"));

        let check = check_full_disk_access(&fs, Path::new(HOME));

        assert_eq!(check.state, AccessState::Missing);
        assert_eq!(check.probe_path, Some(Path::new(HOME).join("Library/Mail")));
    }

    #[test]
    fn the_first_protected_folder_found_decides_even_when_a_later_one_lists() {
        let fs = home_with(PROTECTED[0])
            .with_denied(Path::new(HOME).join(PROTECTED[0]))
            .with_dir(Path::new(HOME).join("Library/Mail"));

        assert_eq!(check_full_disk_access(&fs, Path::new(HOME)).state, AccessState::Missing);
    }

    #[test]
    fn a_protected_path_that_is_not_a_folder_cannot_tell() {
        let fs = FakeFileOps::new().with_root("/", 1).with_file(Path::new(HOME).join(PROTECTED[0]), b"x");

        let check = check_full_disk_access(&fs, Path::new(HOME));

        assert_eq!(check.state, AccessState::Unknown, "{check:?}");
    }

    #[test]
    fn a_home_with_none_of_them_cannot_tell() {
        let fs = FakeFileOps::new().with_root("/", 1).with_dir(HOME);

        assert_eq!(
            check_full_disk_access(&fs, Path::new(HOME)),
            AccessCheck { state: AccessState::Unknown, probe_path: None }
        );
    }
}
