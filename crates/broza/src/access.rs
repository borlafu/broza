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

/// List the first protected folder that exists under `home` and say what
/// macOS answered.
pub fn check_full_disk_access(fs: &dyn FileOps, home: &Path) -> AccessCheck {
    let Some(probe) = PROTECTED.iter().map(|relative| home.join(relative)).find(|path| fs.exists(path))
    else {
        return AccessCheck { state: AccessState::Unknown, probe_path: None };
    };
    let state = match fs.read_dir(&probe) {
        Ok(_) => AccessState::Granted,
        Err(BrozaError::PermissionDenied { .. }) => AccessState::Missing,
        Err(_) => AccessState::Unknown,
    };
    AccessCheck { state, probe_path: Some(probe) }
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
    fn a_home_with_none_of_them_cannot_tell() {
        let fs = FakeFileOps::new().with_root("/", 1).with_dir(HOME);

        assert_eq!(
            check_full_disk_access(&fs, Path::new(HOME)),
            AccessCheck { state: AccessState::Unknown, probe_path: None }
        );
    }
}
