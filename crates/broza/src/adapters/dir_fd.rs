//! Directories by descriptor: the calls that never see a full path.
//!
//! The kernel refuses any path of `PATH_MAX` (1024) bytes or more with
//! `ENAMETOOLONG`, while directories can be *created* deeper than that with
//! short relative names. A walk that opens and stats by path therefore stops
//! at that depth and reports what is below as unreadable. The `*at` calls take
//! a directory descriptor and one entry name instead, so a walk that descends
//! by descriptor has no depth limit but the descriptor table
//! ([`raise_open_files_limit`]). `getattrlistbulk` already works on a
//! descriptor; this file supplies the rest: `openat`, `fstatat`,
//! `getattrlistat` (ADR 0010).

#![allow(unsafe_code)]

use std::ffi::{CString, OsStr};
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::OnceLock;

use jiff::Timestamp;

use crate::BrozaError;
use crate::adapters::io_error::from_io;
use crate::ports::EntryMetadata;

/// Size of the blocks `st_blocks` counts, fixed at 512 bytes by POSIX.
const STAT_BLOCK_BYTES: u64 = 512;
/// `SF_DATALESS` in `st_flags`: a cloud placeholder whose contents are elsewhere.
const SF_DATALESS: u32 = 0x4000_0000;
/// Flags every directory is opened with: only a directory (`O_DIRECTORY`
/// keeps a FIFO from turning an open into a wait), never through a symlink,
/// never blocking, never inherited by a child process.
const DIRECTORY_FLAGS: libc::c_int =
    libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
/// `OPEN_MAX`: the most descriptors macOS lets a process raise its soft limit
/// to when the hard limit is unlimited.
const OPEN_MAX: libc::rlim_t = 10_240;
/// Bytes the kernel writes for one `u64` attribute: the length word, then the value.
const ATTR_REPLY_LEN: usize = 4 + 8;

/// Open the directory at `path`, and only a directory, without blocking on anything.
pub(crate) fn open_directory(path: &Path) -> std::io::Result<File> {
    raise_open_files_limit();
    OpenOptions::new().read(true).custom_flags(DIRECTORY_FLAGS).open(path)
}

/// Open the directory `name` directly inside `parent`, the same way.
pub(crate) fn open_directory_in(parent: &File, name: &OsStr) -> std::io::Result<File> {
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
fn retrying(mut syscall: impl FnMut() -> libc::c_int) -> std::io::Result<libc::c_int> {
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

/// The APFS clone id of the regular file `name` inside `parent`, without
/// following a symlink; `None` when the filesystem reports none.
fn clone_id_in(parent: &File, c_name: &CString) -> Option<u64> {
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: libc::ATTR_CMNEXT_CLONEID,
    };
    let mut reply = [0_u8; ATTR_REPLY_LEN];
    // SAFETY: `c_name` is valid for the call, `request` is the fixed-size
    // struct libc declares, `reply` is a live buffer of the length handed to
    // the kernel, and `parent` is open.
    let status = unsafe {
        libc::getattrlistat(
            parent.as_raw_fd(),
            c_name.as_ptr(),
            std::ptr::from_mut(&mut request).cast(),
            reply.as_mut_ptr().cast(),
            reply.len(),
            u64::from(libc::FSOPT_NOFOLLOW | libc::FSOPT_ATTR_CMN_EXTENDED),
        )
    };
    if status != 0 {
        return None;
    }
    let length = u32::from_ne_bytes(reply[..4].try_into().ok()?) as usize;
    if length < ATTR_REPLY_LEN {
        return None;
    }
    Some(u64::from_ne_bytes(reply[4..ATTR_REPLY_LEN].try_into().ok()?))
}

/// Raise the soft limit on open descriptors as far as the hard limit allows,
/// once per process.
///
/// A walk by descriptor holds one open directory per level of the chain each
/// thread is descending, and a terminal's default soft limit is 256 on some
/// macOS setups. The hard limit stays what it is; `OPEN_MAX` caps the request
/// when the hard limit is unlimited, as the kernel demands. Failure is
/// ignored: a descriptor that cannot be opened becomes a warning in the walk.
pub(crate) fn raise_open_files_limit() {
    static RAISED: OnceLock<()> = OnceLock::new();
    RAISED.get_or_init(|| {
        let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        // SAFETY: `limit` is a live `rlimit` the kernel fills in.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
            return;
        }
        let wanted =
            if limit.rlim_max == libc::RLIM_INFINITY { OPEN_MAX } else { limit.rlim_max.min(OPEN_MAX) };
        if limit.rlim_cur >= wanted {
            return;
        }
        let raised = libc::rlimit { rlim_cur: wanted, rlim_max: limit.rlim_max };
        // SAFETY: `raised` is a valid `rlimit`; the call only changes this
        // process's own soft limit.
        let _ = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const raised) };
    });
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use super::{metadata_in, open_directory, open_directory_in, raise_open_files_limit};
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
    }

    #[test]
    fn raising_the_descriptor_limit_is_idempotent() {
        raise_open_files_limit();
        raise_open_files_limit();
    }
}
