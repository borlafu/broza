//! Path canonicalisation without following symlinks, and the root allowlist.
//!
//! `docs/cli-spec.md` §3.4, checks 2 and 4. Both functions are total: they never
//! touch the filesystem except through [`FileOps`], and they never follow a link.

use std::path::{Component, Path, PathBuf};

use crate::model::{Category, VolumeRole};
use crate::ports::FileOps;
use crate::safety::rejection::GuardRejection;
use crate::safety::roles::is_protected;

/// Shared folder every user can write to.
pub const SHARED_ROOT: &str = "/Users/Shared";
/// System-wide cache folder.
pub const LIBRARY_CACHES_ROOT: &str = "/Library/Caches";
/// Applications folder; only reachable for the `unused-apps` category.
pub const APPLICATIONS_ROOT: &str = "/Applications";
/// Per-volume trash directory name.
pub const TRASHES_DIR: &str = ".Trashes";
/// Mount point of the Data volume; `/Users/x` and `/System/Volumes/Data/Users/x`
/// are the same directory seen through a firmlink.
pub const DATA_VOLUME_ROOT: &str = "/System/Volumes/Data";

/// Roots under which Broza is allowed to remove things.
///
/// The roots themselves are never removable: only strict descendants are allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedRoots {
    roots: Vec<PathBuf>,
}

impl AllowedRoots {
    /// Builds the allowlist from the user's home and the per-uid temporary
    /// directories (`/private/var/folders/<xx>/<uid dir>`), plus the two constants
    /// of the specification.
    ///
    /// Every root is registered under both of its spellings, the firmlinked one
    /// (`/Users/dana`) and the one on the Data volume
    /// (`/System/Volumes/Data/Users/dana`), because they name the same directory.
    pub fn new(home: &Path, uid_temp_dirs: Vec<PathBuf>) -> Self {
        let fixed = [home.to_path_buf(), PathBuf::from(SHARED_ROOT), PathBuf::from(LIBRARY_CACHES_ROOT)];
        let roots = fixed
            .into_iter()
            .chain(uid_temp_dirs)
            .flat_map(|root| {
                let twin = data_volume_twin(&root);
                [root, twin]
            })
            .collect();
        Self { roots }
    }

    /// The configured roots, in declaration order.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }
}

/// The same directory spelled on the Data volume; unchanged when it already is.
fn data_volume_twin(root: &Path) -> PathBuf {
    if root.starts_with(DATA_VOLUME_ROOT) {
        return root.to_path_buf();
    }
    root.strip_prefix("/")
        .map_or_else(|_| root.to_path_buf(), |relative| Path::new(DATA_VOLUME_ROOT).join(relative))
}

/// What the item being checked is, so the conditional roots can be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootContext {
    /// Category of the finding the path came from.
    pub category: Category,
    /// Role of the volume the path resolves to.
    pub role: VolumeRole,
}

/// Normalises `path` lexically and proves that no component is a symbolic link.
///
/// Rejects relative paths, `..` that climbs above the root, and any component that
/// `lstat` reports as a symlink. Every component must exist: the guard refuses to
/// reason about a path it cannot stat.
pub fn canonicalize_no_follow(path: &Path, fs: &dyn FileOps) -> Result<PathBuf, GuardRejection> {
    let normalized = normalize_lexically(path)?;
    reject_symlinked_components(&normalized, fs)?;
    Ok(normalized)
}

/// Resolves `.` and `..` without touching the filesystem.
fn normalize_lexically(path: &Path) -> Result<PathBuf, GuardRejection> {
    if !path.is_absolute() {
        return Err(GuardRejection::NotAbsolute(path.to_path_buf()));
    }
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) | Component::CurDir => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(GuardRejection::EscapesRoot(path.to_path_buf()));
                }
            }
            Component::Normal(part) => parts.push(part),
        }
    }
    Ok(parts.iter().fold(PathBuf::from("/"), |joined, part| joined.join(part)))
}

/// `lstat`s every component from the root down and refuses the first symlink.
fn reject_symlinked_components(path: &Path, fs: &dyn FileOps) -> Result<(), GuardRejection> {
    let root = Path::new("/");
    let mut ancestors: Vec<&Path> = path.ancestors().filter(|entry| *entry != root).collect();
    ancestors.reverse();
    for ancestor in ancestors {
        let metadata = fs.metadata(ancestor).map_err(|error| GuardRejection::Unreadable {
            path: ancestor.to_path_buf(),
            reason: error.to_string(),
        })?;
        if metadata.is_symlink {
            return Err(GuardRejection::SymlinkComponent(ancestor.to_path_buf()));
        }
    }
    Ok(())
}

/// `true` when `path` is a strict descendant of a root Broza is allowed to touch.
///
/// Three groups of roots: the fixed ones in [`AllowedRoots`], the per-volume
/// `.Trashes` directories (never on a protected volume) and `/Applications`, which
/// only the `unused-apps` category may reach (`docs/cli-spec.md` §3.4, check 4).
pub fn is_under_allowed_root(path: &Path, roots: &AllowedRoots, context: RootContext) -> bool {
    if roots.roots().iter().any(|root| is_strictly_under(path, root)) {
        return true;
    }
    if !is_protected(context.role) && is_inside_a_trashes_directory(path) {
        return true;
    }
    context.category == Category::UnusedApps && is_strictly_under(path, Path::new(APPLICATIONS_ROOT))
}

/// `true` when `path` *is* one of the roots, rather than something inside one.
///
/// Used to tell "you asked to remove `$HOME`" apart from "you asked to remove
/// something Broza does not manage".
pub fn is_allowed_root(path: &Path, roots: &AllowedRoots) -> bool {
    roots.roots().iter().any(|root| root == path)
        || path == Path::new(APPLICATIONS_ROOT)
        || path.file_name() == Some(TRASHES_DIR.as_ref())
}

fn is_strictly_under(path: &Path, root: &Path) -> bool {
    path != root && path.starts_with(root)
}

/// `true` when some ancestor of `path` is a `.Trashes` directory.
fn is_inside_a_trashes_directory(path: &Path) -> bool {
    path.ancestors().skip(1).any(|ancestor| ancestor.file_name() == Some(TRASHES_DIR.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::{
        APPLICATIONS_ROOT, AllowedRoots, RootContext, canonicalize_no_follow, is_under_allowed_root,
    };
    use crate::model::{Category, VolumeRole};
    use crate::safety::rejection::GuardRejection;
    use crate::safety::test_fs::MemFs;
    use std::path::{Path, PathBuf};

    const HOME: &str = "/Users/dana";

    fn roots() -> AllowedRoots {
        AllowedRoots::new(Path::new(HOME), vec![PathBuf::from("/private/var/folders/aa/bbb")])
    }

    fn context(category: Category, role: VolumeRole) -> RootContext {
        RootContext { category, role }
    }

    fn data(category: Category) -> RootContext {
        context(category, VolumeRole::Data)
    }

    fn tree() -> MemFs {
        MemFs::new()
            .dir("/Users/dana/Library/Caches")
            .file("/Users/dana/Library/Caches/app.cache", 10)
            .symlink("/Users/dana/Library/Caches/link")
            .symlink("/Users/dana/linked")
            .file("/Users/dana/linked/inside.cache", 10)
    }

    #[test]
    fn a_relative_path_is_refused() {
        let error = canonicalize_no_follow(Path::new("Library/Caches"), &tree());
        assert_eq!(error, Err(GuardRejection::NotAbsolute("Library/Caches".into())));
    }

    #[test]
    fn dot_and_dot_dot_are_resolved_lexically() {
        let resolved =
            canonicalize_no_follow(Path::new("/Users/dana/Library/../Library/./Caches/app.cache"), &tree());
        assert_eq!(resolved, Ok(PathBuf::from("/Users/dana/Library/Caches/app.cache")));
    }

    #[test]
    fn dot_dot_above_the_root_is_refused() {
        let error = canonicalize_no_follow(Path::new("/Users/../../etc"), &tree());
        assert_eq!(error, Err(GuardRejection::EscapesRoot("/Users/../../etc".into())));
    }

    #[test]
    fn a_symlinked_leaf_is_refused() {
        let error = canonicalize_no_follow(Path::new("/Users/dana/Library/Caches/link"), &tree());
        assert_eq!(error, Err(GuardRejection::SymlinkComponent("/Users/dana/Library/Caches/link".into())));
    }

    #[test]
    fn a_symlinked_intermediate_component_is_refused_by_name() {
        let error = canonicalize_no_follow(Path::new("/Users/dana/linked/inside.cache"), &tree());
        assert_eq!(error, Err(GuardRejection::SymlinkComponent("/Users/dana/linked".into())));
    }

    #[test]
    fn a_path_that_cannot_be_stat_ed_is_refused() {
        let error = canonicalize_no_follow(Path::new("/Users/dana/gone"), &tree());
        assert!(
            matches!(error, Err(GuardRejection::Unreadable { .. })),
            "expected an unreadable rejection, got {error:?}"
        );
    }

    #[test]
    fn paths_inside_the_home_and_the_fixed_roots_are_allowed() {
        let allowed = [
            "/Users/dana/Library/Caches/app.cache",
            "/Users/Shared/build",
            "/Library/Caches/com.apple.thing",
            "/private/var/folders/aa/bbb/T/tmp1",
        ];
        for path in allowed {
            assert!(is_under_allowed_root(Path::new(path), &roots(), data(Category::UserCache)), "{path}");
        }
    }

    #[test]
    fn a_root_is_allowed_under_both_of_its_firmlinked_spellings() {
        let path = Path::new("/System/Volumes/Data/Users/dana/Library/Caches/app.cache");
        assert!(is_under_allowed_root(path, &roots(), data(Category::UserCache)));
        assert_eq!(roots().roots().len(), 8, "four roots, two spellings each");
    }

    #[test]
    fn a_root_itself_is_never_allowed() {
        for path in [HOME, "/Users/Shared", "/Library/Caches", "/private/var/folders/aa/bbb"] {
            assert!(!is_under_allowed_root(Path::new(path), &roots(), data(Category::UserCache)), "{path}");
        }
    }

    #[test]
    fn paths_outside_every_root_are_refused() {
        for path in ["/etc/passwd", "/Users/other/Documents", "/System/Library/Caches/x", "/tmp/x"] {
            assert!(!is_under_allowed_root(Path::new(path), &roots(), data(Category::UserCache)), "{path}");
        }
    }

    #[test]
    fn trashes_are_allowed_on_non_system_volumes_only() {
        let path = Path::new("/Volumes/External/.Trashes/501/old.dmg");
        assert!(is_under_allowed_root(path, &roots(), context(Category::Trash, VolumeRole::User)));
        assert!(!is_under_allowed_root(path, &roots(), context(Category::Trash, VolumeRole::System)));
        assert!(!is_under_allowed_root(path, &roots(), context(Category::Trash, VolumeRole::Vm)));
    }

    #[test]
    fn the_trashes_directory_itself_is_not_removable() {
        let path = Path::new("/Volumes/External/.Trashes");
        assert!(!is_under_allowed_root(path, &roots(), context(Category::Trash, VolumeRole::User)));
    }

    #[test]
    fn the_roots_themselves_are_recognised_as_such() {
        for path in [HOME, "/Users/Shared", "/Library/Caches", "/Applications", "/Volumes/X/.Trashes"] {
            assert!(super::is_allowed_root(Path::new(path), &roots()), "{path}");
        }
        for path in ["/Users/dana/Library", "/Applications/Old.app", "/etc"] {
            assert!(!super::is_allowed_root(Path::new(path), &roots()), "{path}");
        }
    }

    #[test]
    fn applications_are_only_reachable_for_unused_apps() {
        let app = Path::new("/Applications/Old.app");
        assert!(is_under_allowed_root(app, &roots(), data(Category::UnusedApps)));
        assert!(!is_under_allowed_root(app, &roots(), data(Category::BuildCache)));
        assert!(!is_under_allowed_root(Path::new(APPLICATIONS_ROOT), &roots(), data(Category::UnusedApps)));
    }
}
