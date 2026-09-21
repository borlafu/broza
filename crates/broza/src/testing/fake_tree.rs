//! The in-memory tree behind [`FakeFileOps`](super::FakeFileOps).
//!
//! Paths are stored verbatim: the tree resolves no `.`, `..`, or trailing slash, so a
//! test must use the same spelling it asked about. Canonicalization is the safety
//! kernel's job, not the filesystem's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;

/// Device reported for paths that are under no declared root.
pub(crate) const DEFAULT_DEVICE: u64 = 1;
/// Allocation unit sizes are rounded up to, mirroring an APFS block.
pub(crate) const ALLOCATION_UNIT_BYTES: u64 = 4096;
/// Link count of a directory with no subdirectories, as on any unix filesystem.
pub(crate) const EMPTY_DIR_LINK_COUNT: u64 = 2;
/// Instant new entries are stamped with.
const DEFAULT_TIME: &str = "2026-01-01T00:00:00Z";

/// What an entry of the tree is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NodeKind {
    /// A regular file and its contents.
    File(Vec<u8>),
    /// A directory.
    Dir,
    /// A symbolic link and its target, which is never resolved by `metadata`.
    Symlink(PathBuf),
}

/// One entry of the tree.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    /// What the entry is.
    pub kind: NodeKind,
    /// Inode number, unique within the tree.
    pub inode: u64,
    /// Apparent size, when the test wants one that does not match the contents.
    pub size_override: Option<u64>,
    /// Last modification time.
    pub modified: Option<Timestamp>,
    /// Last access time.
    pub accessed: Option<Timestamp>,
}

impl Node {
    /// Apparent size in bytes, as `lstat` would report it.
    pub fn size_bytes(&self) -> u64 {
        if let Some(size) = self.size_override {
            return size;
        }
        match &self.kind {
            NodeKind::File(contents) => contents.len() as u64,
            NodeKind::Dir => 0,
            NodeKind::Symlink(target) => target.as_os_str().len() as u64,
        }
    }

    /// Allocated size in bytes: whole allocation units for files, nothing else.
    pub fn allocated_bytes(&self) -> u64 {
        match self.kind {
            NodeKind::File(_) => self.size_bytes().div_ceil(ALLOCATION_UNIT_BYTES) * ALLOCATION_UNIT_BYTES,
            NodeKind::Dir | NodeKind::Symlink(_) => 0,
        }
    }
}

/// Why a change to the tree is impossible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TreeError {
    /// A component of the path exists and is not a directory.
    NotADirectory(PathBuf),
    /// The path itself is already taken by something that is not a directory.
    AlreadyExists(PathBuf),
}

/// An in-memory filesystem tree.
#[derive(Debug, Default)]
pub(crate) struct Tree {
    /// Declared roots and the device each one sits on, in insertion order.
    roots: Vec<(PathBuf, u64)>,
    /// Every entry, keyed by absolute path and kept sorted.
    nodes: BTreeMap<PathBuf, Node>,
    /// Prefixes the fake refuses access to, as a volume without Full Disk Access.
    denied: Vec<PathBuf>,
    /// Inode of the next entry created.
    next_inode: u64,
}

impl Tree {
    /// An empty tree.
    pub fn new() -> Self {
        Self { roots: Vec::new(), nodes: BTreeMap::new(), denied: Vec::new(), next_inode: 1 }
    }

    /// Declare `path` as a root sitting on `device`, creating it as a directory.
    pub fn add_root(&mut self, path: &Path, device: u64) {
        self.roots.push((path.to_path_buf(), device));
        self.insert(path, NodeKind::Dir);
    }

    /// Device of `path`: the device of its longest matching root.
    pub fn device_for(&self, path: &Path) -> u64 {
        self.roots
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .map_or(DEFAULT_DEVICE, |(_, device)| *device)
    }

    /// Entry at `path`, if any.
    pub fn get(&self, path: &Path) -> Option<&Node> {
        self.nodes.get(path)
    }

    /// Refuse every access to `path` and everything below it.
    pub fn add_denied(&mut self, path: &Path) {
        self.denied.push(path.to_path_buf());
    }

    /// `true` when `path` is inside a prefix the test declared unreadable.
    pub fn is_denied(&self, path: &Path) -> bool {
        self.denied.iter().any(|prefix| path.starts_with(prefix))
    }

    /// `true` when something exists at `path`.
    pub fn exists(&self, path: &Path) -> bool {
        self.nodes.contains_key(path)
    }

    /// `true` when `path` is a directory in the tree.
    pub fn is_dir(&self, path: &Path) -> bool {
        matches!(self.get(path).map(|node| &node.kind), Some(NodeKind::Dir))
    }

    /// Insert or replace the entry at `path`.
    ///
    /// Overwriting keeps the inode, which makes a rewritten file recognisable to a
    /// test. Replacing a directory with anything else drops its children first and
    /// takes a fresh inode: the old directory is gone, not renamed.
    pub fn insert(&mut self, path: &Path, kind: NodeKind) {
        let replaced_directory = self.is_dir(path) && !matches!(kind, NodeKind::Dir);
        if replaced_directory {
            self.remove_subtree(path);
        }
        let inode = self.nodes.get(path).map_or_else(
            || {
                let inode = self.next_inode;
                self.next_inode += 1;
                inode
            },
            |node| node.inode,
        );
        let now = default_time();
        let node = Node { kind, inode, size_override: None, modified: now, accessed: now };
        self.nodes.insert(path.to_path_buf(), node);
    }

    /// Create `path` and every missing ancestor as a directory.
    ///
    /// Fails the way `mkdir` does: a component that is not a directory is
    /// [`TreeError::NotADirectory`], and a `path` already taken by a file is
    /// [`TreeError::AlreadyExists`]. An existing directory is success.
    pub fn create_dir_all(&mut self, path: &Path) -> Result<(), TreeError> {
        let mut ancestors: Vec<&Path> = path.ancestors().collect();
        ancestors.reverse();
        for ancestor in ancestors {
            match self.nodes.get(ancestor).map(|node| matches!(node.kind, NodeKind::Dir)) {
                Some(true) => {}
                Some(false) if ancestor == path => {
                    return Err(TreeError::AlreadyExists(path.to_path_buf()));
                }
                Some(false) => return Err(TreeError::NotADirectory(ancestor.to_path_buf())),
                None => self.insert(ancestor, NodeKind::Dir),
            }
        }
        Ok(())
    }

    /// `true` when `path` is a directory with no children.
    pub fn is_empty_dir(&self, path: &Path) -> bool {
        self.is_dir(path) && self.children(path).is_empty()
    }

    /// Set the times of an existing entry.
    pub fn set_times(&mut self, path: &Path, modified: Timestamp, accessed: Timestamp) {
        if let Some(node) = self.nodes.get_mut(path) {
            node.modified = Some(modified);
            node.accessed = Some(accessed);
        }
    }

    /// Give an existing entry an apparent size unrelated to its contents.
    pub fn set_size(&mut self, path: &Path, size_bytes: u64) {
        if let Some(node) = self.nodes.get_mut(path) {
            node.size_override = Some(size_bytes);
        }
    }

    /// Direct children of `path`, sorted.
    pub fn children(&self, path: &Path) -> Vec<PathBuf> {
        self.nodes.keys().filter(|child| child.parent() == Some(path)).cloned().collect()
    }

    /// Remove `path` and everything below it.
    pub fn remove_subtree(&mut self, path: &Path) {
        self.nodes.retain(|key, _| key != path && !key.starts_with(path));
    }

    /// Move `path` and everything below it to `destination`.
    pub fn move_subtree(&mut self, path: &Path, destination: &Path) {
        let moved: Vec<PathBuf> = self.nodes.keys().filter(|key| key.starts_with(path)).cloned().collect();
        for key in moved {
            let Some(node) = self.nodes.remove(&key) else { continue };
            let target = match key.strip_prefix(path) {
                Ok(suffix) if suffix.as_os_str().is_empty() => destination.to_path_buf(),
                Ok(suffix) => destination.join(suffix),
                Err(_) => continue,
            };
            self.nodes.insert(target, node);
        }
    }

    /// Add `link` as another name for the entry at `existing`.
    ///
    /// Both names share one inode, as `link(2)` does. Directories cannot be hard
    /// linked, and a missing source is a no-op; both report `false`.
    pub fn link(&mut self, existing: &Path, link: &Path) -> bool {
        let Some(node) = self.nodes.get(existing).cloned() else { return false };
        if matches!(node.kind, NodeKind::Dir) {
            return false;
        }
        self.nodes.insert(link.to_path_buf(), node);
        true
    }

    /// Link count: the unix convention for directories, names sharing an inode
    /// for everything else.
    pub fn link_count(&self, path: &Path) -> u64 {
        if !self.is_dir(path) {
            let Some(node) = self.get(path) else { return 1 };
            return self.nodes.values().filter(|other| other.inode == node.inode).count() as u64;
        }
        let subdirectories = self.children(path).iter().filter(|child| self.is_dir(child)).count() as u64;
        EMPTY_DIR_LINK_COUNT + subdirectories
    }

    /// Every path in the tree, sorted.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.nodes.keys().cloned().collect()
    }
}

/// Instant new entries are stamped with, or `None` if the constant is unparsable.
fn default_time() -> Option<Timestamp> {
    DEFAULT_TIME.parse::<Timestamp>().ok()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{ALLOCATION_UNIT_BYTES, EMPTY_DIR_LINK_COUNT, NodeKind, Tree};

    fn tree() -> Tree {
        let mut tree = Tree::new();
        tree.add_root(Path::new("/a"), 7);
        let _ = tree.create_dir_all(Path::new("/a/b"));
        tree.insert(Path::new("/a/b/f"), NodeKind::File(b"1234".to_vec()));
        tree
    }

    #[test]
    fn the_device_of_a_path_comes_from_its_longest_root() {
        let mut tree = Tree::new();
        tree.add_root(Path::new("/"), 1);
        tree.add_root(Path::new("/System/Volumes/Data"), 2);

        assert_eq!(tree.device_for(Path::new("/usr/bin")), 1);
        assert_eq!(tree.device_for(Path::new("/System/Volumes/Data/Users")), 2);
        assert_eq!(tree.device_for(Path::new("relative")), super::DEFAULT_DEVICE);
    }

    #[test]
    fn replacing_an_entry_keeps_its_inode() {
        let mut tree = tree();
        let before = tree.get(Path::new("/a/b/f")).map(|node| node.inode);

        tree.insert(Path::new("/a/b/f"), NodeKind::File(b"new".to_vec()));

        assert_eq!(tree.get(Path::new("/a/b/f")).map(|node| node.inode), before);
    }

    #[test]
    fn sizes_round_up_to_whole_allocation_units() {
        let tree = tree();
        let node = tree.get(Path::new("/a/b/f")).unwrap_or_else(|| panic!("missing"));

        assert_eq!(node.size_bytes(), 4);
        assert_eq!(node.allocated_bytes(), ALLOCATION_UNIT_BYTES);
    }

    #[test]
    fn an_explicit_size_overrides_the_contents() {
        let mut tree = tree();
        tree.set_size(Path::new("/a/b/f"), 10 * ALLOCATION_UNIT_BYTES);

        let node = tree.get(Path::new("/a/b/f")).unwrap_or_else(|| panic!("missing"));

        assert_eq!(node.size_bytes(), 10 * ALLOCATION_UNIT_BYTES);
        assert_eq!(node.allocated_bytes(), 10 * ALLOCATION_UNIT_BYTES);
    }

    #[test]
    fn a_symlink_is_as_large_as_its_target_path() {
        let mut tree = tree();
        tree.insert(Path::new("/a/link"), NodeKind::Symlink(PathBuf::from("b/f")));

        let node = tree.get(Path::new("/a/link")).unwrap_or_else(|| panic!("missing"));

        assert_eq!(node.size_bytes(), 3);
        assert_eq!(node.allocated_bytes(), 0);
    }

    #[test]
    fn children_are_direct_and_sorted() {
        let mut tree = tree();
        tree.insert(Path::new("/a/z"), NodeKind::File(Vec::new()));

        assert_eq!(tree.children(Path::new("/a")), vec![PathBuf::from("/a/b"), PathBuf::from("/a/z")]);
    }

    #[test]
    fn removing_a_directory_removes_its_descendants() {
        let mut tree = tree();

        tree.remove_subtree(Path::new("/a/b"));

        assert_eq!(tree.paths(), vec![PathBuf::from("/"), PathBuf::from("/a")]);
    }

    #[test]
    fn moving_a_directory_moves_its_descendants() {
        let mut tree = tree();

        tree.move_subtree(Path::new("/a/b"), Path::new("/a/c"));

        assert_eq!(
            tree.paths(),
            vec![PathBuf::from("/"), PathBuf::from("/a"), PathBuf::from("/a/c"), PathBuf::from("/a/c/f"),]
        );
    }

    #[test]
    fn directories_count_their_subdirectories_as_links() {
        let tree = tree();

        assert_eq!(tree.link_count(Path::new("/a")), EMPTY_DIR_LINK_COUNT + 1);
        assert_eq!(tree.link_count(Path::new("/a/b")), EMPTY_DIR_LINK_COUNT);
        assert_eq!(tree.link_count(Path::new("/a/b/f")), 1);
    }

    #[test]
    fn times_can_be_pinned_for_a_test() {
        let mut tree = tree();
        let when = "2020-02-02T02:02:02Z".parse().unwrap_or_else(|e| panic!("{e}"));

        tree.set_times(Path::new("/a/b/f"), when, when);

        assert_eq!(tree.get(Path::new("/a/b/f")).and_then(|node| node.modified), Some(when));
    }
}
