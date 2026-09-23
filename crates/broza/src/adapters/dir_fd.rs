//! Directories by descriptor: the calls that never see a full path.
//!
//! The kernel refuses any path of `PATH_MAX` (1024) bytes or more with
//! `ENAMETOOLONG`, while directories can be *created* deeper than that with
//! short relative names. A walk that opens and stats by path therefore stops
//! at that depth and reports what is below as unreadable. The `*at` calls take
//! a directory descriptor and one entry name instead, so a directory deeper
//! than a path can name is reached by opening the deepest ancestor whose path
//! still fits and stepping down one name at a time ([`open_directory`]), and
//! its children are stated by name inside it ([`metadata_in`]).
//! `getattrlistbulk` already works on a descriptor; this file supplies the
//! rest: `openat`, `fstatat`, `fdopendir` (ADR 0010).
//!
//! One descriptor is held per directory being listed, never a chain: the
//! walk's descriptor use is what it was when it opened by path.

#![allow(unsafe_code)]

use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use jiff::Timestamp;

use crate::BrozaError;
use crate::adapters::clone_id::clone_id_in;
use crate::adapters::io_error::from_io;
use crate::ports::EntryMetadata;

/// Size of the blocks `st_blocks` counts, fixed at 512 bytes by POSIX.
pub(crate) const STAT_BLOCK_BYTES: u64 = 512;
/// `SF_DATALESS` in `st_flags`: a cloud placeholder whose contents are elsewhere.
pub(crate) const SF_DATALESS: u32 = 0x4000_0000;
/// Flags every directory is opened with: only a directory (`O_DIRECTORY`
/// keeps a FIFO from turning an open into a wait), never through a symlink,
/// never blocking, never inherited by a child process.
const DIRECTORY_FLAGS: libc::c_int =
    libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;

/// Open the directory at `path`, and only a directory, without blocking on anything.
///
/// A path the kernel can name is opened as one call. When the kernel answers
/// `ENAMETOOLONG` — the string itself is too long, or a symlink on the way
/// (`/var` → `/private/var`) makes what the kernel resolves too long — the
/// deepest ancestor that does open is found by asking, and the rest is opened
/// one relative name at a time, each step through the previous descriptor and
/// closing it. So the depth costs calls, never descriptors.
pub(crate) fn open_directory(path: &Path) -> std::io::Result<File> {
    let (anchor, mut dir) = open_deepest_ancestor(path)?;
    let rest =
        path.strip_prefix(anchor).map_err(|_| std::io::Error::from_raw_os_error(libc::ENAMETOOLONG))?;
    for step in rest.components() {
        dir = open_directory_in(&dir, step.as_os_str())?;
    }
    Ok(dir)
}

/// `path` itself when it opens; otherwise the deepest ancestor that does, when
/// the only thing wrong with the ones below was their length.
fn open_deepest_ancestor(path: &Path) -> std::io::Result<(&Path, File)> {
    let mut too_long = None;
    for ancestor in path.ancestors() {
        match open_by_path(ancestor) {
            Ok(dir) => return Ok((ancestor, dir)),
            Err(failed) if failed.raw_os_error() == Some(libc::ENAMETOOLONG) => too_long = Some(failed),
            // Anything else — missing, denied, not a directory — is about the
            // path as asked, and is reported as such.
            Err(failed) => return Err(too_long.unwrap_or(failed)),
        }
    }
    Err(too_long.unwrap_or_else(|| std::io::Error::from_raw_os_error(libc::ENAMETOOLONG)))
}

/// One `open` of a directory by path.
fn open_by_path(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).custom_flags(DIRECTORY_FLAGS).open(path)
}

/// Open the directory `name` directly inside `parent`, the same way.
fn open_directory_in(parent: &File, name: &OsStr) -> std::io::Result<File> {
    let c_name = CString::new(name.as_bytes())?;
    // SAFETY: `c_name` is a valid NUL-terminated string that outlives the
    // call, and `parent` is an open descriptor for as long as the borrow.
    let fd = retrying(|| unsafe {
        libc::openat(parent.as_raw_fd(), c_name.as_ptr(), libc::O_RDONLY | DIRECTORY_FLAGS)
    })?;
    // SAFETY: `fd` was just returned by `openat` and is owned by nobody else.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Call `syscall` again while it fails with `EINTR`, as `std` does for its
/// own calls: a signal (a window resize, a child exiting) interrupts a raw
/// call and is not a reason to report a directory unreadable.
pub(crate) fn retrying(mut syscall: impl FnMut() -> libc::c_int) -> std::io::Result<libc::c_int> {
    loop {
        let result = syscall();
        if result >= 0 {
            return Ok(result);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// `lstat` of the entry `name` directly inside `parent`, as [`EntryMetadata`].
///
/// `path` is what the entry is called in an error, never what is opened.
pub(crate) fn metadata_in(parent: &File, name: &OsStr, path: &Path) -> Result<EntryMetadata, BrozaError> {
    let c_name = CString::new(name.as_bytes())
        .map_err(|source| from_io(format!("stat {}", path.display()), path, source.into()))?;
    // SAFETY: an all-zero `struct stat` is a valid value of a plain C struct.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `c_name` is valid for the call, `stat` is a live, correctly
    // sized `struct stat` the kernel fills in, and `parent` is open.
    let stated = retrying(|| unsafe {
        libc::fstatat(parent.as_raw_fd(), c_name.as_ptr(), &raw mut stat, libc::AT_SYMLINK_NOFOLLOW)
    });
    if let Err(source) = stated {
        return Err(from_io(format!("stat {}", path.display()), path, source));
    }
    let kind = stat.st_mode & libc::S_IFMT;
    let clone_id = if kind == libc::S_IFREG { clone_id_in(parent, &c_name) } else { None };
    Ok(entry_metadata(&stat, kind, clone_id))
}

/// [`EntryMetadata`] from a `struct stat`, field for field as `std` reads it.
#[expect(
    clippy::cast_sign_loss,
    reason = "matching std::os::unix::fs::MetadataExt, which widens the signed st_dev and reads st_size/st_blocks as u64"
)]
fn entry_metadata(stat: &libc::stat, kind: libc::mode_t, clone_id: Option<u64>) -> EntryMetadata {
    EntryMetadata {
        device: i64::from(stat.st_dev) as u64,
        inode: stat.st_ino,
        size_bytes: stat.st_size as u64,
        allocated_bytes: (stat.st_blocks as u64).saturating_mul(STAT_BLOCK_BYTES),
        link_count: u64::from(stat.st_nlink),
        is_dir: kind == libc::S_IFDIR,
        is_symlink: kind == libc::S_IFLNK,
        is_dataless: stat.st_flags & SF_DATALESS != 0,
        modified: timestamp(stat.st_mtime, stat.st_mtime_nsec),
        accessed: timestamp(stat.st_atime, stat.st_atime_nsec),
        clone_id,
    }
}

/// A `timespec` as a timestamp, `None` when out of range.
fn timestamp(seconds: i64, nanoseconds: i64) -> Option<Timestamp> {
    Timestamp::new(seconds, i32::try_from(nanoseconds).ok()?).ok()
}

/// The names of the entries of the open directory `dir`, as `readdir` lists
/// them, without `.` and `..`.
///
/// The plain pair's first half, for when the bulk reader will not answer: it
/// reads through a duplicate of the descriptor (`fdopendir` takes ownership of
/// the one it is given) and never hands the kernel a path.
pub(crate) fn read_dir_names_in(dir: &File) -> std::io::Result<Vec<OsString>> {
    // SAFETY: `dir` is an open descriptor; `dup` returns a fresh one or -1.
    let duplicate = retrying(|| unsafe { libc::dup(dir.as_raw_fd()) })?;
    // SAFETY: `duplicate` is a fresh descriptor this call owns; `fdopendir`
    // takes it over on success and leaves it to us on failure.
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: `duplicate` is still ours to close; `fdopendir` failed.
        unsafe { libc::close(duplicate) };
        return Err(error);
    }
    let mut names = Vec::new();
    loop {
        // SAFETY: `stream` is a live `DIR*` until `closedir` below; the
        // returned entry is valid until the next call on the same stream.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        // SAFETY: `entry` points at a `dirent` the kernel filled in; `d_name`
        // is NUL-terminated within its `d_namlen` bytes.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        let bytes = name.to_bytes();
        if bytes != b"." && bytes != b".." {
            names.push(OsString::from_vec(bytes.to_vec()));
        }
    }
    // SAFETY: `stream` came from `fdopendir` and is closed exactly once.
    unsafe { libc::closedir(stream) };
    Ok(names)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use super::{metadata_in, open_directory, open_directory_in, read_dir_names_in};
    use crate::adapters::StdFileOps;
    use crate::ports::FileOps;

    #[test]
    fn a_child_stated_by_descriptor_matches_one_stated_by_path() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        std::fs::create_dir(dir.path().join("sub")).unwrap_or_else(|e| panic!("mkdir: {e}"));
        std::fs::write(dir.path().join("sub/file"), b"hello").unwrap_or_else(|e| panic!("write: {e}"));
        std::os::unix::fs::symlink("file", dir.path().join("sub/link")).unwrap_or_else(|e| panic!("ln: {e}"));
        let root = open_directory(dir.path()).unwrap_or_else(|e| panic!("open: {e}"));
        let sub = open_directory_in(&root, OsStr::new("sub")).unwrap_or_else(|e| panic!("openat: {e}"));

        for name in ["file", "link"] {
            let path = dir.path().join("sub").join(name);
            let by_fd = metadata_in(&sub, OsStr::new(name), &path).unwrap_or_else(|e| panic!("{e}"));
            let by_path = StdFileOps.metadata(&path).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(by_fd, by_path, "{name}");
        }
        let sub_meta =
            metadata_in(&root, OsStr::new("sub"), &dir.path().join("sub")).unwrap_or_else(|e| panic!("{e}"));
        assert!(sub_meta.is_dir);
        assert_eq!(
            sub_meta.inode,
            std::fs::metadata(dir.path().join("sub")).unwrap_or_else(|e| panic!("{e}")).ino()
        );
        assert!(metadata_in(&sub, OsStr::new("missing"), Path::new("/x/missing")).is_err());
        assert!(open_directory_in(&sub, OsStr::new("file")).is_err(), "a file is not a directory");
        let mut names = read_dir_names_in(&sub).unwrap_or_else(|e| panic!("readdir: {e}"));
        names.sort();
        assert_eq!(names, vec![OsStr::new("file"), OsStr::new("link")]);
        // The stream read through a duplicate: the descriptor is still usable.
        assert!(metadata_in(&sub, OsStr::new("file"), Path::new("/x/file")).is_ok());
    }
}
