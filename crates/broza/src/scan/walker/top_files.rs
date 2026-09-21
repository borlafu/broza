//! The biggest files of a walk, kept in bounded memory.
//!
//! `--min-size 0` on a home directory would otherwise collect millions of
//! entries to then throw all but twenty away. Each branch of the walk keeps at
//! most `cap` files in a heap and drops the smallest as it goes; merging two
//! branches keeps the biggest of both.

use std::collections::BinaryHeap;

use crate::scan::walker::FileEntry;

/// The `cap` biggest files seen so far.
#[derive(Debug, Default)]
pub(super) struct TopFiles {
    /// How many to keep; zero keeps none.
    cap: usize,
    /// The kept files, smallest on top so it is the one evicted.
    heap: BinaryHeap<Smallest>,
}

impl TopFiles {
    /// An empty set keeping at most `cap` files.
    pub fn new(cap: usize) -> Self {
        Self { cap, heap: BinaryHeap::new() }
    }

    /// The same set with `entry` considered.
    pub fn with(mut self, entry: FileEntry) -> Self {
        if self.cap == 0 {
            return self;
        }
        self.heap.push(Smallest(entry));
        if self.heap.len() > self.cap {
            let _ = self.heap.pop();
        }
        self
    }

    /// The biggest files of both sets.
    pub fn merge(self, other: Self) -> Self {
        let (mut bigger, smaller) =
            if self.heap.len() >= other.heap.len() { (self, other) } else { (other, self) };
        bigger.cap = bigger.cap.max(smaller.cap);
        for entry in smaller.heap {
            bigger = bigger.with(entry.0);
        }
        bigger
    }

    /// The kept files, in no particular order.
    pub fn into_vec(self) -> Vec<FileEntry> {
        self.heap.into_iter().map(|entry| entry.0).collect()
    }
}

/// A file entry ordered so that the *least* interesting one comes out first.
///
/// "Least interesting" is the smallest, and among equal sizes the one whose path
/// sorts last — so that two runs of the same scan keep the same file.
#[derive(Debug, PartialEq, Eq)]
struct Smallest(FileEntry);

impl Ord for Smallest {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.0.size_bytes.cmp(&self.0.size_bytes).then_with(|| self.0.path.cmp(&other.0.path))
    }
}

impl PartialOrd for Smallest {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::TopFiles;
    use crate::scan::walker::FileEntry;

    fn file(path: &str, size_bytes: u64) -> FileEntry {
        FileEntry { path: PathBuf::from(path), size_bytes, allocated_bytes: size_bytes }
    }

    fn names(files: TopFiles) -> Vec<String> {
        let mut names: Vec<String> =
            files.into_vec().into_iter().map(|file| file.path.display().to_string()).collect();
        names.sort();
        names
    }

    #[test]
    fn only_the_biggest_files_are_kept() {
        let kept =
            [file("/a", 1), file("/b", 5), file("/c", 3)].into_iter().fold(TopFiles::new(2), TopFiles::with);

        assert_eq!(names(kept), vec!["/b".to_owned(), "/c".to_owned()]);
    }

    #[test]
    fn a_set_that_keeps_nothing_keeps_nothing() {
        let kept = TopFiles::new(0).with(file("/a", 10));

        assert!(kept.into_vec().is_empty());
    }

    #[test]
    fn merging_two_branches_keeps_the_biggest_of_both() {
        let left = TopFiles::new(2).with(file("/a", 1)).with(file("/b", 9));
        let right = TopFiles::new(2).with(file("/c", 4)).with(file("/d", 7));

        assert_eq!(names(left.merge(right)), vec!["/b".to_owned(), "/d".to_owned()]);
    }

    #[test]
    fn among_files_of_the_same_size_the_first_path_survives() {
        let kept = [file("/z", 4), file("/a", 4)].into_iter().fold(TopFiles::new(1), TopFiles::with);

        assert_eq!(names(kept), vec!["/a".to_owned()]);
    }

    #[test]
    fn merging_into_an_empty_set_keeps_the_other_sides_limit() {
        let kept = TopFiles::new(0).merge(TopFiles::new(2).with(file("/a", 1)).with(file("/b", 2)));

        assert_eq!(names(kept), vec!["/a".to_owned(), "/b".to_owned()]);
    }
}
