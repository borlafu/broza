//! The APFS clone id of one file, through `getattrlist(2)` or `getattrlistat(2)`.
//!
//! `lstat` has no field for it: the clone id lives in the extended common
//! attributes (`ATTR_CMNEXT_CLONEID`), which only `getattrlist` and
//! `getattrlistbulk` report, and only when asked with
//! `FSOPT_ATTR_CMN_EXTENDED`. The bulk reader asks for it along with
//! everything else; this file is the one-file counterpart the plain adapters
//! use, by path ([`clone_id_of`]) or by name inside an open directory
//! ([`clone_id_in`]), so every reader describes a file the same way — the bulk
//! reader's cross-check (`bulk_dir`) depends on that, and one request and one
//! reply layout serve both.
//!
//! Measured on macOS 26 (APFS), pinned by `tests/fakes_behave_like_std.rs`:
//! a file that was never cloned reports its own inode; every clone made from
//! it (`clonefile(2)`, `cp -c`, the Finder's duplicate) reports the original's
//! inode; a hard link shares the inode and so the clone id. A directory
//! reports its inode too, which says nothing useful, so callers ask for
//! regular files only.

#![allow(unsafe_code)]

use std::ffi::{CStr, CString};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::adapters::dir_fd::retrying;

/// Bytes the kernel writes for one `u64` attribute: the length word, then the value.
const REPLY_LEN: usize = 4 + 8;
/// Byte offset of the value inside the reply.
const OFF_VALUE: usize = 4;
/// Options of every request: never follow a symlink, read the extended set.
const OPTIONS: u32 = libc::FSOPT_NOFOLLOW | libc::FSOPT_ATTR_CMN_EXTENDED;

/// The clone id of the regular file at `path`, without following a symlink.
///
/// `None` when the filesystem does not report one (not APFS, or a path that
/// vanished): the caller then knows nothing about clones, rather than
/// believing a zero.
pub(crate) fn clone_id_of(path: &Path) -> Option<u64> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut request = request();
    let mut reply = [0_u8; REPLY_LEN];
    // SAFETY: `c_path` is a valid NUL-terminated string that outlives the
    // call, `request` is the fixed-size struct libc declares, and `reply` is a
    // live buffer of `REPLY_LEN` bytes, which is the size handed to the
    // kernel; it writes no more than that.
    retrying(|| unsafe {
        libc::getattrlist(
            c_path.as_ptr(),
            std::ptr::from_mut(&mut request).cast(),
            reply.as_mut_ptr().cast(),
            reply.len(),
            OPTIONS,
        )
    })
    .ok()?;
    parse_reply(&reply)
}

/// The clone id of the regular file `name` directly inside `dir`, likewise.
pub(crate) fn clone_id_in(dir: &File, name: &CStr) -> Option<u64> {
    let mut request = request();
    let mut reply = [0_u8; REPLY_LEN];
    // SAFETY: `name` is a valid NUL-terminated string for the call, `dir` is
    // an open descriptor, `request` is the fixed-size struct libc declares,
    // and `reply` is a live buffer of the length handed to the kernel.
    retrying(|| unsafe {
        libc::getattrlistat(
            dir.as_raw_fd(),
            name.as_ptr(),
            std::ptr::from_mut(&mut request).cast(),
            reply.as_mut_ptr().cast(),
            reply.len(),
            u64::from(OPTIONS),
        )
    })
    .ok()?;
    parse_reply(&reply)
}

/// The one attribute both calls ask for.
fn request() -> libc::attrlist {
    libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: libc::ATTR_CMNEXT_CLONEID,
    }
}

/// The value out of a reply, `None` when the kernel returned no attribute.
fn parse_reply(reply: &[u8; REPLY_LEN]) -> Option<u64> {
    let length = u32::from_ne_bytes(reply[..OFF_VALUE].try_into().ok()?) as usize;
    if length < REPLY_LEN {
        return None;
    }
    Some(u64::from_ne_bytes(reply[OFF_VALUE..REPLY_LEN].try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;

    use super::{clone_id_in, clone_id_of};

    #[test]
    fn a_fresh_file_reports_its_own_inode_by_path_and_by_name_and_a_missing_one_nothing() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let file = dir.path().join("fresh");
        std::fs::write(&file, b"x").unwrap_or_else(|e| panic!("write: {e}"));
        let inode = std::fs::metadata(&file).unwrap_or_else(|e| panic!("stat: {e}")).ino();
        let handle = std::fs::File::open(dir.path()).unwrap_or_else(|e| panic!("open: {e}"));
        let name = CString::new("fresh").unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(clone_id_of(&file), Some(inode));
        assert_eq!(clone_id_in(&handle, &name), Some(inode));
        assert_eq!(clone_id_of(&dir.path().join("missing")), None);
    }
}
