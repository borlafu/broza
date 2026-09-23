//! The directory-handle half of [`StdFileOps`](super::StdFileOps): open a
//! directory as a descriptor and list it through that descriptor.
//!
//! Kept apart from `std_fs.rs` so both files stay small. The walker calls
//! [`open_dir`] then [`list_dir`] for every directory it measures; nothing in
//! between hands the kernel a full path, which is what lets a tree deeper than
//! `PATH_MAX` be walked to the bottom (ADR 0010).

use std::any::Any;
use std::fs::File;
use std::path::Path;

use rayon::iter::{IntoParallelIterator, ParallelIterator};

use crate::BrozaError;
use crate::adapters::io_error::from_io;
use crate::adapters::{bulk_dir, dir_fd};
use crate::ports::{DirHandle, DirListing};

/// An open directory descriptor, the handle the walker lists through.
#[derive(Debug)]
pub(super) struct FdDirHandle(File);

impl DirHandle for FdDirHandle {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The descriptor behind a handle this adapter handed out.
///
/// `None` for a handle another [`FileOps`](crate::ports::FileOps) made, which
/// no caller does today: a decorator that forwards `open_dir` must hand back
/// the inner handle, or the depth guarantee is quietly off.
fn descriptor(handle: &dyn DirHandle) -> Option<&File> {
    let found = handle.as_any().downcast_ref::<FdDirHandle>().map(|handle| &handle.0);
    debug_assert!(found.is_some(), "a directory handle StdFileOps did not make");
    found
}

/// Open the directory at `path`, however deep, as a descriptor.
pub(super) fn open_dir(path: &Path) -> Result<Box<dyn DirHandle>, BrozaError> {
    let dir = dir_fd::open_directory(path)
        .map_err(|source| from_io(format!("open directory {}", path.display()), path, source))?;
    Ok(Box::new(FdDirHandle(dir)))
}

/// Open the directory `relative` below the open `anchor`, one relative step at
/// a time; by path when the anchor is not one of ours.
pub(super) fn open_dir_below(
    anchor: &dyn DirHandle,
    relative: &Path,
    path: &Path,
) -> Result<Box<dyn DirHandle>, BrozaError> {
    let Some(anchor) = descriptor(anchor) else {
        return open_dir(path);
    };
    let dir = dir_fd::open_directory_below(anchor, relative)
        .map_err(|source| from_io(format!("open directory {}", path.display()), path, source))?;
    Ok(Box::new(FdDirHandle(dir)))
}

/// `(device, inode)` of the open directory.
pub(super) fn dir_identity(dir: &dyn DirHandle) -> Option<(u64, u64)> {
    descriptor(dir).and_then(dir_fd::identity_of)
}

/// The children of the open directory `dir`, each with its metadata, named
/// under `path`.
///
/// The bulk reader answers when it can; otherwise the plain pair, through the
/// same descriptor: `readdir` on a fresh descriptor of the same directory,
/// `fstatat` per entry, spread over the pool because each `fstatat` waits on
/// the disk. That
/// spreading lets a waiting thread pick up another directory's work while this
/// descriptor is held, so in fallback mode descriptors can nest on one thread;
/// the bulk path, the ordinary one, is sequential.
pub(super) fn list_dir(
    dir: &dyn DirHandle,
    path: &Path,
    fallback: impl Fn() -> Result<DirListing, BrozaError>,
) -> Result<DirListing, BrozaError> {
    let Some(dir) = descriptor(dir) else {
        return fallback();
    };
    if let Some(listing) = bulk_dir::read_dir_in(dir, path) {
        return Ok(listing);
    }
    let names = dir_fd::read_dir_names_in(dir)
        .map_err(|source| from_io(format!("read directory {}", path.display()), path, source))?;
    Ok(names
        .into_par_iter()
        .map(|name| {
            let child = path.join(&name);
            let meta = dir_fd::metadata_in(dir, &name, &child);
            (child, meta)
        })
        .collect())
}
