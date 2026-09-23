//! Directories by descriptor: the calls that never see a full path.
//!
//! The kernel refuses any path of `PATH_MAX` (1024) bytes or more with
//! `ENAMETOOLONG`, while directories can be *created* deeper than that with
//! short relative names. A walk that opens and stats by path therefore stops
//! at that depth and reports what is below as unreadable. The `*at` calls take
//! a directory descriptor and one entry name instead, so a directory deeper
//! than a path can name is reached by opening the deepest ancestor whose path
//! still fits and the rest relative to it, a stretch of names at a time
//! ([`open_directory`]), and
//! its children are stated by name inside it ([`metadata_in`]).
//! `getattrlistbulk` already works on a descriptor; this file supplies the
//! rest: `openat`, `fstatat`, `fdopendir` (ADR 0010).
//!
//! One descriptor is held per directory being listed, plus one anchor every
//! few dozen levels once paths grow past what the kernel can name: the walk's
//! descriptor use is what it was when it opened by path, within a handful.

#![allow(unsafe_code)]

use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::BrozaError;
use crate::adapters::clone_id::clone_id_in;
use crate::adapters::io_error::from_io;
use crate::ports::EntryMetadata;

/// Size of the blocks `st_blocks` counts, fixed at 512 bytes by POSIX.
pub(crate) const STAT_BLOCK_BYTES: u64 = 512;
/// `SF_DATALESS` in `st_flags`: a cloud placeholder whose contents are elsewhere.
pub(crate) const SF_DATALESS: u32 = 0x4000_0000;
/// `PATH_MAX`: the kernel refuses a path of this many bytes or more. A
/// shorter string can still be refused when a symlink on the way lengthens
/// what the kernel resolves, so this is a shortcut, not the test.
pub(crate) const PATH_MAX_BYTES: usize = 1024;
/// Flags every directory is opened with: only a directory (`O_DIRECTORY`
/// keeps a FIFO from turning an open into a wait), never through a symlink,
/// never blocking, never inherited by a child process.
const DIRECTORY_FLAGS: libc::c_int =
    libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
/// Flags a relative path below an open directory is opened with: as above,
/// but no symlink is followed in *any* component (`O_NOFOLLOW_ANY`, which the
/// kernel refuses together with `O_NOFOLLOW`), so a chain of names never
/// leaves the directory it started from.
const RELATIVE_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW_ANY | libc::O_NONBLOCK | libc::O_CLOEXEC;

/// Open the directory at `path`, and only a directory, without blocking on anything.
///
/// A path the kernel can name is opened as one call. When the kernel answers
/// `ENAMETOOLONG` — the string itself is too long, or a symlink on the way
/// (`/var` → `/private/var`) makes what the kernel resolves too long — the
/// deepest ancestor that does open is found by asking, and the rest is opened
/// relative to it, a stretch of names per call, each intermediate descriptor
/// closed. So the depth costs calls, never descriptors.
pub(crate) fn open_directory(path: &Path) -> std::io::Result<File> {
    let (anchor, dir) = open_deepest_ancestor(path)?;
    let rest =
        path.strip_prefix(anchor).map_err(|_| std::io::Error::from_raw_os_error(libc::ENAMETOOLONG))?;
    if rest.as_os_str().is_empty() {
        return Ok(dir);
    }
    open_directory_below(&dir, rest)
}

/// `path` itself when it opens; otherwise the deepest ancestor that does, when
/// the only thing wrong with the ones below was their length.
fn open_deepest_ancestor(path: &Path) -> std::io::Result<(&Path, File)> {
    let mut too_long = None;
    for ancestor in path.ancestors() {
        // A string the kernel cannot take is not worth a call.
        if ancestor.as_os_str().len() >= PATH_MAX_BYTES {
            continue;
        }
        match open_by_path(ancestor) {
            Ok(dir) => return Ok((ancestor, dir)),
            Err(failed) if failed.raw_os_error() == Some(libc::ENAMETOOLONG) => too_long = Some(failed),
            // Anything else — missing, denied, not a directory — is about that
            // ancestor, and climbing further would not get past it: the way
            // down leads through it again. Its own error is the answer.
            Err(failed) => return Err(failed),
        }
    }
    Err(too_long.unwrap_or_else(|| std::io::Error::from_raw_os_error(libc::ENAMETOOLONG)))
}

/// One `open` of a directory by path.
fn open_by_path(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).custom_flags(DIRECTORY_FLAGS).open(path)
}

/// Open the directory `relative` steps below the open `anchor`, in as few
/// calls as the kernel allows: one `openat` per stretch of the relative path
/// shorter than `PATH_MAX`, no symlink followed anywhere, each intermediate
/// descriptor closed. Short names come dozens to a call; only `Normal`
/// components are accepted, since `..` or an absolute one would leave the
/// anchor behind.
pub(crate) fn open_directory_below(anchor: &File, relative: &Path) -> std::io::Result<File> {
    let mut dir: Option<File> = None;
    let mut stretch = PathBuf::new();
    for step in relative.components() {
        let std::path::Component::Normal(name) = step else {
            return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
        };
        let would_be = stretch.as_os_str().len().saturating_add(1).saturating_add(name.len());
        if !stretch.as_os_str().is_empty() && would_be >= PATH_MAX_BYTES {
            dir = Some(open_relative(dir.as_ref().unwrap_or(anchor), &stretch)?);
            stretch = PathBuf::new();
        }
        stretch.push(name);
    }
    if stretch.as_os_str().is_empty() {
        stretch.push(".");
    }
    open_relative(dir.as_ref().unwrap_or(anchor), &stretch)
}

/// One `openat` of the directory `relative` below `parent`.
fn open_relative(parent: &File, relative: &Path) -> std::io::Result<File> {
    let c_path = CString::new(relative.as_os_str().as_bytes())?;
    // SAFETY: `c_path` is a valid NUL-terminated string that outlives the
    // call, and `parent` is an open descriptor for as long as the borrow.
    let fd = retrying(|| unsafe { libc::openat(parent.as_raw_fd(), c_path.as_ptr(), RELATIVE_FLAGS) })?;
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
    reason = "matching std::os::unix::fs::MetadataExt, which reads st_size and st_blocks as u64"
)]
fn entry_metadata(stat: &libc::stat, kind: libc::mode_t, clone_id: Option<u64>) -> EntryMetadata {
    EntryMetadata {
        device: device_of(stat),
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
    // `fdopendir` takes ownership of the descriptor it is given and reads
    // from where that descriptor stands, which the bulk reader has moved to
    // the end: so a close-on-exec duplicate, rewound with `lseek`, which does
    // move what `readdir` sees (measured, pinned by `bulk_dir::tests`). No
    // reopen: that would need search permission and, deep in a tree, another
    // costly path lookup.
    // SAFETY: `dir` is an open descriptor; `fcntl` returns a fresh one or -1.
    let duplicate = retrying(|| unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) })?;
    // SAFETY: `duplicate` is a descriptor this call owns until `fdopendir`
    // takes it over.
    if unsafe { libc::lseek(duplicate, 0, libc::SEEK_SET) } < 0 {
        let error = std::io::Error::last_os_error();
        // SAFETY: `duplicate` is still ours to close.
        unsafe { libc::close(duplicate) };
        return Err(error);
    }
    // SAFETY: `duplicate` is ours; `fdopendir` takes it over on success and
    // leaves it to us on failure.
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: `duplicate` is still ours to close; `fdopendir` failed.
        unsafe { libc::close(duplicate) };
        return Err(error);
    }
    let names = read_names(stream);
    // SAFETY: `stream` came from `fdopendir` and is closed exactly once.
    unsafe { libc::closedir(stream) };
    names
}

/// `(device, inode)` of the open directory, to check it is the one the
/// listing named: opening by path follows symlinks in every component but the
/// last, so an ancestor swapped for a link between the listing and the open
/// would otherwise be walked under the wrong identity.
pub(crate) fn identity_of(dir: &File) -> Option<(u64, u64)> {
    // SAFETY: an all-zero `struct stat` is a valid value of a plain C struct.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `dir` is open and `stat` is a live, correctly sized struct.
    retrying(|| unsafe { libc::fstat(dir.as_raw_fd(), &raw mut stat) }).ok()?;
    Some((device_of(&stat), stat.st_ino))
}

/// A device id the way `std` widens it: `dev_t` is signed.
#[expect(clippy::cast_sign_loss, reason = "matching std::os::unix::fs::MetadataExt::dev")]
fn device_of(stat: &libc::stat) -> u64 {
    i64::from(stat.st_dev) as u64
}

/// Every name in the stream but `.` and `..`; an error partway through is an
/// error, not a shorter directory (`readdir` reports one as NULL with errno
/// set, and NULL with errno clear at the end).
fn read_names(stream: *mut libc::DIR) -> std::io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    loop {
        // SAFETY: `__error()` is the thread's errno slot; writing it is allowed.
        unsafe { *libc::__error() = 0 };
        // SAFETY: `stream` is a live `DIR*` until the caller's `closedir`; the
        // returned entry is valid until the next call on the same stream.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(0) {
                return Ok(names);
            }
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        // SAFETY: `entry` points at a `dirent` the kernel filled in; `d_name`
        // is NUL-terminated within its `d_namlen` bytes.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        let bytes = name.to_bytes();
        if bytes != b"." && bytes != b".." {
            names.push(OsString::from_vec(bytes.to_vec()));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use std::path::PathBuf;

    use super::{
        PATH_MAX_BYTES, identity_of, metadata_in, open_directory, open_directory_below, read_dir_names_in,
    };
    use crate::adapters::StdFileOps;
    use crate::ports::FileOps;

    #[test]
    fn a_child_stated_by_descriptor_matches_one_stated_by_path() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        std::fs::create_dir(dir.path().join("sub")).unwrap_or_else(|e| panic!("mkdir: {e}"));
        std::fs::write(dir.path().join("sub/file"), b"hello").unwrap_or_else(|e| panic!("write: {e}"));
        std::os::unix::fs::symlink("file", dir.path().join("sub/link")).unwrap_or_else(|e| panic!("ln: {e}"));
        let root = open_directory(dir.path()).unwrap_or_else(|e| panic!("open: {e}"));
        let sub = open_directory_below(&root, Path::new("sub")).unwrap_or_else(|e| panic!("openat: {e}"));

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
        assert!(open_directory_below(&sub, Path::new("file")).is_err(), "a file is not a directory");
        let mut names = read_dir_names_in(&sub).unwrap_or_else(|e| panic!("readdir: {e}"));
        names.sort();
        assert_eq!(names, vec![OsStr::new("file"), OsStr::new("link")]);
        // Listing again on the same descriptor sees everything again: the
        // stream has its own offset, whatever an earlier reader moved.
        let mut again = read_dir_names_in(&sub).unwrap_or_else(|e| panic!("readdir: {e}"));
        again.sort();
        assert_eq!(again, names);
        assert!(metadata_in(&sub, OsStr::new("file"), Path::new("/x/file")).is_ok());
        assert_eq!(
            identity_of(&sub),
            Some((sub_meta.device, sub_meta.inode)),
            "the descriptor is the directory the listing named"
        );
        std::fs::create_dir_all(dir.path().join("sub/x/y")).unwrap_or_else(|e| panic!("mkdir: {e}"));
        std::os::unix::fs::symlink("x", dir.path().join("sub/xlink")).unwrap_or_else(|e| panic!("ln: {e}"));
        let deep = open_directory_below(&root, Path::new("sub/x/y")).unwrap_or_else(|e| panic!("{e}"));
        let by_path = open_directory(&dir.path().join("sub/x/y")).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(identity_of(&deep), identity_of(&by_path), "several names in one step");
        assert!(
            open_directory_below(&root, Path::new("sub/xlink/y")).is_err(),
            "a symlink in the middle is not followed either (O_NOFOLLOW alone would follow it)"
        );
        assert!(open_directory_below(&root, Path::new("sub/../sub")).is_err(), "no way up or out");
        assert!(open_directory_below(&root, Path::new("/")).is_err());
    }

    #[test]
    fn a_relative_path_longer_than_the_kernel_takes_is_opened_in_stretches() {
        // Five names of 255 bytes: 1279 bytes relative, more than one call
        // takes, so the path is opened in stretches the kernel accepts. The
        // tree is built one relative name at a time, as the kernel forces.
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let name = "n".repeat(255);
        let built = std::process::Command::new("sh")
            .arg("-c")
            .arg("cd \"$1\" && for i in 1 2 3 4 5; do mkdir \"$2\" && cd \"$2\"; done")
            .arg("sh")
            .arg(dir.path())
            .arg(&name)
            .status()
            .unwrap_or_else(|e| panic!("sh: {e}"));
        assert!(built.success(), "building the tree failed: {built}");
        let relative: PathBuf = (0..5).map(|_| name.as_str()).collect();
        assert!(
            relative.as_os_str().len() > PATH_MAX_BYTES,
            "the relative path alone is too long for one call"
        );
        let root = open_directory(dir.path()).unwrap_or_else(|e| panic!("open: {e}"));

        let stretched = open_directory_below(&root, &relative).unwrap_or_else(|e| panic!("{e}"));
        let mut step_by_step =
            open_directory_below(&root, Path::new(&name)).unwrap_or_else(|e| panic!("{e}"));
        for _ in 1..5 {
            step_by_step =
                open_directory_below(&step_by_step, Path::new(&name)).unwrap_or_else(|e| panic!("{e}"));
        }

        assert_eq!(identity_of(&stretched), identity_of(&step_by_step));
    }
}
