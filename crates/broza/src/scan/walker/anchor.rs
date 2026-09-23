//! Opening a directory for the walk: by path, or below an anchor once paths
//! outgrow what the kernel can name, and never under the wrong identity.
//!
//! Paths longer than `PATH_MAX` (1024 bytes) are opened by relative steps
//! (ADR 0010). From the top of the tree each time that is quadratic in the
//! depth past the limit — a review measured 35 s for eight chains of 600
//! levels — so once a path is [`ANCHOR_FROM_BYTES`] long the walk keeps one
//! handle every [`ANCHOR_EVERY_LEVELS`] levels as the [`Anchor`] the
//! directories below are opened from: a bounded number of steps per directory,
//! one descriptor per that many levels on each chain being walked, none on an
//! ordinary tree.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::BrozaError;
use crate::ports::{DirHandle, EntryMetadata};
use crate::scan::walker::parts::Context;

/// Paths from this many bytes on are opened below an anchor: enough room
/// before the kernel's limit that a symlink on the way does not push a path
/// that looked short over it.
pub(super) const ANCHOR_FROM_BYTES: usize = 768;
/// A new anchor is kept this many levels below the previous one.
pub(super) const ANCHOR_EVERY_LEVELS: usize = 32;

/// An open directory kept as the starting point for the directories below it.
pub(super) struct Anchor {
    /// The open directory.
    handle: Box<dyn DirHandle>,
    /// Its path, which every directory below it starts with.
    path: PathBuf,
    /// How many levels deep it is.
    depth: usize,
}

impl Anchor {
    /// The anchor the children of `path` are opened from: `dir` itself when
    /// the path has grown long and the previous anchor is far enough up,
    /// otherwise the previous one (or none, which drops `dir` here).
    pub fn for_children(
        path: &Path,
        dir: Box<dyn DirHandle>,
        previous: Option<Arc<Self>>,
    ) -> Option<Arc<Self>> {
        if path.as_os_str().len() < ANCHOR_FROM_BYTES {
            return None;
        }
        let depth = path.components().count();
        let due =
            previous.as_ref().is_none_or(|anchor| depth.saturating_sub(anchor.depth) >= ANCHOR_EVERY_LEVELS);
        if due { Some(Arc::new(Self { handle: dir, path: path.to_path_buf(), depth })) } else { previous }
    }
}

/// Open `path` for listing and make sure it is the directory `meta` describes.
///
/// Below an anchor when there is one, by path otherwise. Opening by path
/// follows symlinks in every component but the last, so an ancestor swapped
/// for a link between the listing and the open would hand back another
/// directory; its `(device, inode)` is compared with the listing's, and a
/// mismatch is an error the caller reports, never a walk under the wrong
/// identity.
pub(super) fn open_checked(
    path: &Path,
    meta: &EntryMetadata,
    anchor: Option<&Anchor>,
    context: &Context<'_>,
) -> Result<Box<dyn DirHandle>, BrozaError> {
    let relative = anchor.and_then(|anchor| path.strip_prefix(&anchor.path).ok().map(|rel| (anchor, rel)));
    let dir = match relative {
        Some((anchor, relative)) => context.fs.open_dir_below(anchor.handle.as_ref(), relative, path)?,
        None => context.fs.open_dir(path)?,
    };
    match context.fs.dir_identity(dir.as_ref()) {
        Some(found) if found != (meta.device, meta.inode) => Err(replaced(path)),
        _ => Ok(dir),
    }
}

/// The error a directory earns when the descriptor opened for it is not the
/// one the listing named.
fn replaced(path: &Path) -> BrozaError {
    BrozaError::Io {
        context: format!("open directory {}", path.display()),
        source: std::io::Error::other("replaced while scanning"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use super::{ANCHOR_EVERY_LEVELS, ANCHOR_FROM_BYTES, Anchor};
    use crate::ports::{DirHandle, PathDirHandle};

    fn handle(path: &Path) -> Box<dyn DirHandle> {
        Box::new(PathDirHandle(path.to_path_buf()))
    }

    /// A path of `levels` components, each a `name` long, from the root.
    fn deep(levels: usize, name: &str) -> PathBuf {
        let mut path = PathBuf::from("/");
        for _ in 0..levels {
            path.push(name);
        }
        path
    }

    #[test]
    fn a_short_path_keeps_no_anchor_and_drops_the_handle() {
        let path = deep(10, "d");
        assert!(path.as_os_str().len() < ANCHOR_FROM_BYTES);

        assert!(Anchor::for_children(&path, handle(&path), None).is_none());
    }

    #[test]
    fn a_long_path_becomes_the_anchor_and_is_replaced_only_every_so_many_levels() {
        let first = deep(400, "d");
        assert!(first.as_os_str().len() >= ANCHOR_FROM_BYTES);

        let anchor = Anchor::for_children(&first, handle(&first), None).unwrap_or_else(|| panic!("kept"));
        assert_eq!(anchor.path, first);
        assert_eq!(anchor.depth, 401, "the root counts as a component");

        let soon = deep(400 + ANCHOR_EVERY_LEVELS - 1, "d");
        let same = Anchor::for_children(&soon, handle(&soon), Some(Arc::clone(&anchor)))
            .unwrap_or_else(|| panic!("kept"));
        assert!(Arc::ptr_eq(&same, &anchor), "too close to the previous anchor");

        let due = deep(400 + ANCHOR_EVERY_LEVELS, "d");
        let next = Anchor::for_children(&due, handle(&due), Some(anchor)).unwrap_or_else(|| panic!("kept"));
        assert_eq!(next.path, due);
    }
}
