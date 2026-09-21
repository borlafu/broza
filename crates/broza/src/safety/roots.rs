//! The root allowlist (`docs/cli-spec.md` §3.4, check 4).
//!
//! Broza removes things *inside* a small set of roots and never the roots
//! themselves. Two of those roots are conditional: the `.Trashes` directory at the
//! root of a writable volume, and `/Applications`, which only the `unused-apps`
//! category may reach.

use std::path::{Component, Path, PathBuf};

use crate::model::Category;
use crate::safety::rejection::GuardRejection;
use crate::safety::roles::is_protected;
use crate::scan::MountEntry;

/// Shared folder every user can write to.
pub const SHARED_ROOT: &str = "/Users/Shared";
/// System-wide cache folder.
pub const LIBRARY_CACHES_ROOT: &str = "/Library/Caches";
/// Applications folder; only reachable for the `unused-apps` category.
pub const APPLICATIONS_ROOT: &str = "/Applications";
/// Per-volume trash directory name, at the root of the volume.
pub const TRASHES_DIR: &str = ".Trashes";
/// Mount point of the Data volume; `/Users/x` and `/System/Volumes/Data/Users/x`
/// are the same directory seen through a firmlink.
pub const DATA_VOLUME_ROOT: &str = "/System/Volumes/Data";
/// Parent of the per-uid temporary directories.
const UID_TEMP_PARENT: &str = "/private/var/folders";
/// `/Users/<name>`: the two components a home directory has.
const HOME_DEPTH: usize = 2;
/// `<xx>/<hash>`: what a per-uid temporary directory adds to its parent.
const UID_TEMP_DEPTH: usize = 2;

/// Roots under which Broza is allowed to remove things.
///
/// The roots themselves are never removable: only strict descendants are allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedRoots {
    roots: Vec<PathBuf>,
}

impl AllowedRoots {
    /// Builds the allowlist from the user's home and the per-uid temporary
    /// directories, plus the two constants of the specification.
    ///
    /// Inputs are validated, because a root that is too broad would open every
    /// path below it: `home` must be `/Users/<name>` and each temporary directory
    /// `/private/var/folders/<xx>/<hash>`. Both may also be spelled on the Data
    /// volume. Every root is registered under both spellings.
    pub fn new(home: &Path, uid_temp_dirs: Vec<PathBuf>) -> Result<Self, GuardRejection> {
        validate_home(home)?;
        for dir in &uid_temp_dirs {
            validate_uid_temp_dir(dir)?;
        }
        let fixed = [home.to_path_buf(), PathBuf::from(SHARED_ROOT), PathBuf::from(LIBRARY_CACHES_ROOT)];
        let roots = fixed
            .into_iter()
            .chain(uid_temp_dirs)
            .flat_map(|root| {
                let twin = data_volume_twin(&root);
                [root, twin]
            })
            .collect();
        Ok(Self { roots })
    }

    /// The configured roots, in declaration order, each in both spellings.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }
}

fn invalid_root(path: &Path, reason: &str) -> GuardRejection {
    GuardRejection::InvalidRoot { path: path.to_path_buf(), reason: reason.to_owned() }
}

/// `home` must be exactly `/Users/<name>`, in either spelling.
fn validate_home(home: &Path) -> Result<(), GuardRejection> {
    let plain = plain_absolute(home)?;
    let parts = normal_components(&plain);
    if parts.len() == HOME_DEPTH && parts[0] == "Users" && parts[1] != "Shared" {
        return Ok(());
    }
    Err(invalid_root(home, "the home directory must be `/Users/<name>`"))
}

/// Each temporary directory must be exactly `/private/var/folders/<xx>/<hash>`.
fn validate_uid_temp_dir(dir: &Path) -> Result<(), GuardRejection> {
    let plain = plain_absolute(dir)?;
    let parts = normal_components(&plain);
    let expected = normal_components(Path::new(UID_TEMP_PARENT));
    if parts.len() == expected.len() + UID_TEMP_DEPTH && parts.starts_with(&expected) {
        return Ok(());
    }
    Err(invalid_root(dir, "a temporary directory must be `/private/var/folders/<xx>/<hash>`"))
}

/// A root has to be absolute and free of `.` and `..`, like any other path.
fn plain_absolute(path: &Path) -> Result<PathBuf, GuardRejection> {
    crate::safety::path::reject_relative_components(path)
        .map_err(|_| invalid_root(path, "must be a plain absolute path"))?;
    Ok(without_data_volume_prefix(path))
}

fn normal_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

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
pub fn firmlink_spellings(path: &Path) -> [PathBuf; 2] {
    let plain = without_data_volume_prefix(path);
    let twin = data_volume_twin(&plain);
    [plain, twin]
}

/// What the item being checked is, so the conditional roots can be resolved.
#[derive(Debug, Clone, Copy)]
pub struct RootContext<'a> {
    /// Category of the finding the path came from.
    pub category: Category,
    /// Volume the path resolves to, from the firmlink-aware mount table.
    pub mount: &'a MountEntry,
}

impl RootContext<'_> {
    /// `<mount point>/.Trashes`, the only trash directory of this volume.
    fn trash_root(&self) -> PathBuf {
        self.mount.mount_point.join(TRASHES_DIR)
    }

    /// Trash is reachable on data and user volumes only, never on a protected one.
    fn trash_is_reachable(&self) -> bool {
        !is_protected(self.mount.volume.role) && self.mount.volume.role.writable_by_broza()
    }
}

/// `true` when `path` is a strict descendant of a root Broza is allowed to touch.
pub fn is_under_allowed_root(path: &Path, roots: &AllowedRoots, context: RootContext) -> bool {
    if roots.roots().iter().any(|root| is_strictly_under(path, root)) {
        return true;
    }
    if context.trash_is_reachable() && is_strictly_under(path, &context.trash_root()) {
        return true;
    }
    context.category == Category::UnusedApps && is_under_either_spelling(path, APPLICATIONS_ROOT)
}

/// `true` when `path` *is* one of the roots, rather than something inside one.
///
/// Used to tell "you asked to remove `$HOME`" apart from "you asked to remove
/// something Broza does not manage".
pub fn is_allowed_root(path: &Path, roots: &AllowedRoots, context: RootContext) -> bool {
    roots.roots().iter().any(|root| root == path)
        || firmlink_spellings(Path::new(APPLICATIONS_ROOT)).contains(&path.to_path_buf())
        || path == context.trash_root()
}

fn is_under_either_spelling(path: &Path, root: &str) -> bool {
    firmlink_spellings(Path::new(root)).iter().any(|spelling| is_strictly_under(path, spelling))
}

fn is_strictly_under(path: &Path, root: &Path) -> bool {
    path != root && path.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::{
        APPLICATIONS_ROOT, AllowedRoots, RootContext, firmlink_spellings, is_allowed_root,
        is_under_allowed_root,
    };
    use crate::model::{Category, Volume, VolumeRole};
    use crate::safety::rejection::GuardRejection;
    use crate::scan::MountEntry;
    use std::path::{Path, PathBuf};

    const HOME: &str = "/Users/dana";

    fn roots() -> AllowedRoots {
        AllowedRoots::new(Path::new(HOME), vec![PathBuf::from("/private/var/folders/aa/bbbbbb")])
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn mount(role: VolumeRole, mount_point: &str) -> MountEntry {
        MountEntry {
            mount_point: PathBuf::from(mount_point),
            device: 2,
            volume: Volume {
                id: "disk3s5".parse().unwrap_or_else(|error| panic!("{error}")),
                name: "test".to_owned(),
                role,
                mount_point: Some(PathBuf::from(mount_point)),
                used_bytes: 0,
                writable_by_broza: role.writable_by_broza(),
                purpose: String::new(),
            },
            firmlinks: Vec::new(),
        }
    }

    fn context(category: Category, mount: &MountEntry) -> RootContext<'_> {
        RootContext { category, mount }
    }

    #[test]
    fn a_root_that_is_too_broad_is_refused() {
        for home in ["/", "/Users", "/Users/Shared", "/private/var", "Users/dana", "/Users/dana/.."] {
            let error = AllowedRoots::new(Path::new(home), Vec::new());
            assert!(
                matches!(error, Err(GuardRejection::InvalidRoot { .. })),
                "{home} must not be a home directory: {error:?}"
            );
        }
        for dir in ["/private/var", "/private/var/folders", "/private/var/folders/aa", "/tmp/x/y"] {
            let error = AllowedRoots::new(Path::new(HOME), vec![PathBuf::from(dir)]);
            assert!(
                matches!(error, Err(GuardRejection::InvalidRoot { .. })),
                "{dir} must not be a temporary root: {error:?}"
            );
        }
    }

    #[test]
    fn both_spellings_of_every_root_are_accepted() {
        let data_home = AllowedRoots::new(Path::new("/System/Volumes/Data/Users/dana"), Vec::new());
        assert!(data_home.is_ok(), "{data_home:?}");
        assert_eq!(roots().roots().len(), 8, "four roots, two spellings each");
        let twin = Path::new("/System/Volumes/Data/Users/dana/Library/Caches/app.cache");
        let data = mount(VolumeRole::Data, "/System/Volumes/Data");
        assert!(is_under_allowed_root(twin, &roots(), context(Category::UserCache, &data)));
    }

    #[test]
    fn paths_inside_the_home_and_the_fixed_roots_are_allowed() {
        let data = mount(VolumeRole::Data, "/System/Volumes/Data");
        let allowed = [
            "/Users/dana/Library/Caches/app.cache",
            "/Users/Shared/build",
            "/Library/Caches/com.apple.thing",
            "/private/var/folders/aa/bbbbbb/T/tmp1",
        ];
        for path in allowed {
            assert!(
                is_under_allowed_root(Path::new(path), &roots(), context(Category::UserCache, &data)),
                "{path}"
            );
        }
    }

    #[test]
    fn a_root_itself_is_never_allowed() {
        let data = mount(VolumeRole::Data, "/System/Volumes/Data");
        for path in [HOME, "/Users/Shared", "/Library/Caches", "/private/var/folders/aa/bbbbbb"] {
            let context = context(Category::UserCache, &data);
            assert!(!is_under_allowed_root(Path::new(path), &roots(), context), "{path}");
            assert!(is_allowed_root(Path::new(path), &roots(), context), "{path}");
        }
    }

    #[test]
    fn paths_outside_every_root_are_refused() {
        let data = mount(VolumeRole::Data, "/System/Volumes/Data");
        for path in ["/etc/passwd", "/Users/other/Documents", "/System/Library/Caches/x", "/tmp/x"] {
            assert!(
                !is_under_allowed_root(Path::new(path), &roots(), context(Category::UserCache, &data)),
                "{path}"
            );
        }
    }

    #[test]
    fn only_the_trash_at_the_root_of_a_writable_volume_is_reachable() {
        let external = mount(VolumeRole::User, "/Volumes/External");
        let at_root = Path::new("/Volumes/External/.Trashes/501/old.dmg");
        assert!(is_under_allowed_root(at_root, &roots(), context(Category::Trash, &external)));

        let system = mount(VolumeRole::System, "/");
        assert!(!is_under_allowed_root(
            Path::new("/.Trashes/501/x"),
            &roots(),
            context(Category::Trash, &system)
        ));

        // A `.Trashes` directory buried anywhere else is not a trash folder.
        let data = mount(VolumeRole::Data, "/System/Volumes/Data");
        let buried = Path::new("/System/Volumes/Data/private/var/db/.Trashes/victim");
        assert!(!is_under_allowed_root(buried, &roots(), context(Category::Trash, &data)));
    }

    #[test]
    fn the_trashes_directory_itself_is_not_removable() {
        let external = mount(VolumeRole::User, "/Volumes/External");
        let path = Path::new("/Volumes/External/.Trashes");
        let context = context(Category::Trash, &external);
        assert!(!is_under_allowed_root(path, &roots(), context));
        assert!(is_allowed_root(path, &roots(), context));
    }

    #[test]
    fn applications_are_only_reachable_for_unused_apps_in_both_spellings() {
        let data = mount(VolumeRole::Data, "/System/Volumes/Data");
        for app in ["/Applications/Old.app", "/System/Volumes/Data/Applications/Old.app"] {
            assert!(
                is_under_allowed_root(Path::new(app), &roots(), context(Category::UnusedApps, &data)),
                "{app}"
            );
            assert!(
                !is_under_allowed_root(Path::new(app), &roots(), context(Category::BuildCache, &data)),
                "{app}"
            );
        }
        let context = context(Category::UnusedApps, &data);
        assert!(!is_under_allowed_root(Path::new(APPLICATIONS_ROOT), &roots(), context));
        assert!(is_allowed_root(Path::new(APPLICATIONS_ROOT), &roots(), context));
    }

    #[test]
    fn a_path_has_exactly_two_spellings() {
        let expected = [PathBuf::from("/Users/dana/x"), PathBuf::from("/System/Volumes/Data/Users/dana/x")];
        assert_eq!(firmlink_spellings(Path::new("/Users/dana/x")), expected);
        assert_eq!(firmlink_spellings(Path::new("/System/Volumes/Data/Users/dana/x")), expected);
    }
}
