//! The two spellings of a path on a Mac with firmlinks.
//!
//! `/Users/dana` and `/System/Volumes/Data/Users/dana` are the same directory:
//! the Data volume is mounted at `/System/Volumes/Data` and firmlinked into `/`.
//! Anything that compares paths — the allowlist, the exclusions, the check that
//! an item belongs to its finding — has to consider both, or the same file gets
//! two different answers depending on how it was spelled.

use std::path::{Path, PathBuf};

use crate::scan::MountTable;

/// Mount point of the Data volume.
pub const DATA_VOLUME_ROOT: &str = "/System/Volumes/Data";

/// The same directory spelled on the Data volume; unchanged when it already is.
pub fn data_volume_twin(path: &Path) -> PathBuf {
    if path.starts_with(DATA_VOLUME_ROOT) {
        return path.to_path_buf();
    }
    path.strip_prefix("/")
        .map_or_else(|_| path.to_path_buf(), |relative| Path::new(DATA_VOLUME_ROOT).join(relative))
}

/// The same directory spelled through the firmlink; unchanged when it already is.
pub fn without_data_volume_prefix(path: &Path) -> PathBuf {
    path.strip_prefix(DATA_VOLUME_ROOT)
        .map_or_else(|_| path.to_path_buf(), |relative| Path::new("/").join(relative))
}

/// Both spellings of a path, the firmlinked one first.
///
/// A volume root has no twin. `/` and `/System/Volumes/Data` are two different
/// volumes, not two names for one directory, and pairing them would make either
/// one a prefix of every path on the machine — which is how a prefix test turns
/// into "anything goes".
pub fn firmlink_spellings(path: &Path) -> [PathBuf; 2] {
    let plain = without_data_volume_prefix(path);
    if plain == Path::new("/") {
        return [path.to_path_buf(), path.to_path_buf()];
    }
    let twin = data_volume_twin(&plain);
    [plain, twin]
}

/// `true` when `path` is a volume root rather than something inside a volume:
/// `/`, a mount point, or a firmlink prefix, in either spelling.
pub fn is_volume_root(path: &Path, mounts: &MountTable) -> bool {
    if path == Path::new("/") {
        return true;
    }
    mounts.entries().iter().any(|entry| {
        std::iter::once(&entry.mount_point)
            .chain(entry.firmlinks.iter())
            .any(|root| firmlink_spellings(root).contains(&path.to_path_buf()))
    })
}

#[cfg(test)]
mod tests {
    use super::{firmlink_spellings, is_volume_root};
    use std::path::{Path, PathBuf};

    /// A volume root is not "one directory under two names": treating `/` and
    /// the Data mount point as twins makes either a prefix of everything.
    #[test]
    fn a_volume_root_has_no_twin() {
        for root in ["/", "/System/Volumes/Data"] {
            assert_eq!(
                firmlink_spellings(Path::new(root)),
                [PathBuf::from(root), PathBuf::from(root)],
                "{root}"
            );
        }
    }

    #[test]
    fn a_path_inside_a_volume_has_exactly_two_spellings() {
        let expected = [PathBuf::from("/Users/dana/x"), PathBuf::from("/System/Volumes/Data/Users/dana/x")];
        assert_eq!(firmlink_spellings(Path::new("/Users/dana/x")), expected);
        assert_eq!(firmlink_spellings(Path::new("/System/Volumes/Data/Users/dana/x")), expected);
    }

    #[test]
    fn the_roots_of_every_mounted_volume_are_recognised() {
        let mounts = crate::testing::mac_mount_table();
        let roots = [
            "/",
            "/System/Volumes/Data",
            "/Users",
            "/System/Volumes/Data/Users",
            "/Applications",
            "/Volumes/External",
        ];
        for root in roots {
            assert!(is_volume_root(Path::new(root), &mounts), "{root}");
        }
        for inside in ["/Users/dana", "/System/Volumes/Data/Users/dana", "/Volumes/External/x"] {
            assert!(!is_volume_root(Path::new(inside), &mounts), "{inside}");
        }
    }
}
