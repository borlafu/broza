//! One directory, one syscall: `getattrlistbulk` instead of `readdir` + `lstat`.
//!
//! A scan of a home directory looks at millions of entries, and the plain pair
//! costs two syscalls each — that is where the seconds of a cold scan go
//! (`docs/cli-spec.md` §7). `getattrlistbulk(2)` returns a whole directory's
//! entries *with* their attributes in one call, which is what every fast
//! du-style tool on macOS uses. [`parse`] reads the buffer; this file decides
//! whether the buffer may be believed at all.
//!
//! # Trusting it
//!
//! Reading attributes by offset out of a kernel buffer cannot be allowed to
//! fail quietly: a misread would not crash, it would report wrong sizes. So:
//!
//! 1. the first directory ever read is cross-checked against `lstat`, entry by
//!    entry, by exactly one thread — everybody else uses the plain pair while
//!    that happens;
//! 2. the reader is trusted only once at least one entry came out of the
//!    buffer itself and matched, so a directory of directories (all of which
//!    fall back to `lstat`) proves nothing and leaves the question open;
//! 3. any disagreement, any implausible entry, any error from the kernel that
//!    is not about this one directory, and the reader is refused for the rest
//!    of the process — a decision that is never taken back.
//!
//! `crates/broza/tests/fakes_behave_like_std.rs` pins both readers together
//! over a scratch tree that includes a resource fork, a FIFO and an empty
//! directory.

#![allow(unsafe_code)]

mod parse;

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::BrozaError;
use crate::ports::{DirListing, EntryMetadata, FileOps};
use parse::ParsedEntry;

/// Bytes handed to the kernel per call; big enough for a large directory.
const BUFFER_BYTES: usize = 256 * 1024;
/// Nothing has been read yet; the next caller cross-checks.
const STATE_UNTESTED: u8 = 0;
/// One thread is cross-checking; everybody else uses the plain pair.
const STATE_TESTING: u8 = 1;
/// The cross-check passed; entries are trusted.
const STATE_TRUSTED: u8 = 2;
/// Something did not add up; the plain pair is used from now on, for good.
const STATE_REFUSED: u8 = 3;

/// Whether the bulk reader may be used.
static BULK_STATE: AtomicU8 = AtomicU8::new(STATE_UNTESTED);

thread_local! {
    /// One buffer per thread, reused across directories.
    ///
    /// A quarter of a megabyte per walker thread, allocated once, instead of
    /// once per directory — a home directory has hundreds of thousands.
    static BUFFER: std::cell::RefCell<Vec<u8>> = std::cell::RefCell::new(vec![0; BUFFER_BYTES]);
}

/// Forget what the reader learned, so a test can exercise it from scratch.
#[cfg(any(test, feature = "test-support"))]
pub fn reset_bulk_state_for_tests() {
    BULK_STATE.store(STATE_UNTESTED, Ordering::SeqCst);
}

/// Read a whole directory with its attributes, or `None` to use the plain pair.
///
/// `None` means "not here, not now": an unsupported filesystem, an error from
/// the kernel, another thread still deciding whether the reader can be trusted,
/// or a buffer that did not read the way [`parse`] expects.
pub(crate) fn read_dir_with_attributes(path: &Path) -> Option<DirListing> {
    match BULK_STATE.load(Ordering::Acquire) {
        STATE_TRUSTED => Some(resolve(path, collect(path)?)),
        STATE_UNTESTED => try_first_directory(path),
        // Refused for good, or somebody else is deciding right now.
        _ => None,
    }
}

/// Read the first directory and decide whether the reader can be trusted.
///
/// Only the thread that wins the claim does this; the rest fall back, because
/// waiting for it would cost more than the `lstat`s they are avoiding.
fn try_first_directory(path: &Path) -> Option<DirListing> {
    if BULK_STATE
        .compare_exchange(STATE_UNTESTED, STATE_TESTING, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return None;
    }
    let Some(parsed) = collect(path) else {
        return refuse();
    };
    let Some(verified) = agrees_with_lstat(path, &parsed) else {
        return refuse();
    };
    // A directory holding nothing the buffer itself described proves nothing —
    // every entry was stated the plain way — so leave the question open.
    let settled = if verified > 0 { STATE_TRUSTED } else { STATE_UNTESTED };
    let _ = BULK_STATE.compare_exchange(STATE_TESTING, settled, Ordering::AcqRel, Ordering::Acquire);
    Some(resolve(path, parsed))
}

/// Every entry of `path` as the buffer described it.
fn collect(path: &Path) -> Option<Vec<ParsedEntry>> {
    let dir = open_directory(path).ok()?;
    let mut request = request_list();
    let mut entries = Vec::new();
    loop {
        let read = BUFFER.with(|buffer| {
            let mut buffer = buffer.borrow_mut();
            let count = call_bulk(&dir, &mut request, &mut buffer);
            let parsed = usize::try_from(count)
                .ok()
                .map_or(Some(Vec::new()), |count| parse::parse_batch(&buffer, count));
            (count, parsed)
        });
        match read {
            (count, _) if count < 0 => return unsupported_or_none(),
            (0, _) => return Some(entries),
            (_, None) => return refuse(),
            (_, Some(parsed)) => entries.extend(parsed),
        }
    }
}

/// One `getattrlistbulk` call; negative means the kernel refused.
fn call_bulk(dir: &File, request: &mut libc::attrlist, buffer: &mut [u8]) -> i32 {
    // SAFETY: `request` is the fixed-size struct libc declares and `buffer` is
    // a live allocation of `buffer.len()` bytes; the kernel writes no more than
    // that. The descriptor belongs to a `File` that outlives the call.
    unsafe {
        libc::getattrlistbulk(
            dir.as_raw_fd(),
            std::ptr::from_mut(request).cast(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            0,
        )
    }
}

/// Open a directory, and only a directory, without blocking on anything.
///
/// `O_DIRECTORY` keeps a FIFO from turning an open into a wait for a writer,
/// `O_NOFOLLOW` keeps a symlink from redirecting the walk, `O_NONBLOCK` is the
/// belt to that braces, and `O_CLOEXEC` keeps the descriptor out of any child
/// process Broza spawns.
fn open_directory(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}

/// The attribute list handed to the kernel.
fn request_list() -> libc::attrlist {
    libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: parse::COMMON_ATTRS,
        volattr: 0,
        dirattr: 0,
        fileattr: parse::FILE_ATTRS,
        forkattr: 0,
    }
}

/// Turn parsed entries into a listing, stating whatever the buffer left out.
fn resolve(parent: &Path, parsed: Vec<ParsedEntry>) -> DirListing {
    parsed
        .into_iter()
        .map(|entry| {
            let path = parent.join(&entry.name);
            let meta = match entry.meta {
                Some(meta) => Ok(meta),
                None => stat(&path),
            };
            (path, meta)
        })
        .collect()
}

/// Metadata of `path` the plain way, through the adapter everything else uses.
fn stat(path: &Path) -> Result<EntryMetadata, BrozaError> {
    crate::adapters::StdFileOps.metadata(path)
}

/// How many entries the buffer described and `lstat` confirms; `None` on any
/// disagreement.
///
/// Entries the parser left to `lstat` are skipped: comparing them against the
/// call they came from proves nothing. An entry that has vanished since the
/// listing is skipped too — a race is not a reason to distrust the kernel.
fn agrees_with_lstat(parent: &Path, parsed: &[ParsedEntry]) -> Option<usize> {
    let mut verified = 0_usize;
    for entry in parsed {
        let Some(meta) = &entry.meta else { continue };
        let Ok(stated) = stat(&parent.join(&entry.name)) else { continue };
        if &stated != meta {
            return None;
        }
        verified += 1;
    }
    Some(verified)
}

/// A kernel that cannot do this at all is refused; one bad directory is not.
///
/// `ENOTSUP` means the filesystem does not implement the call, and no other
/// directory on it will either.
fn unsupported_or_none<T>() -> Option<T> {
    let errno = std::io::Error::last_os_error().raw_os_error();
    if errno == Some(libc::ENOTSUP) {
        return refuse();
    }
    release_claim();
    None
}

/// Give up on the bulk reader for the rest of the process.
fn refuse<T>() -> Option<T> {
    BULK_STATE.store(STATE_REFUSED, Ordering::Release);
    None
}

/// Hand the claim back when a directory could not be read for its own reasons.
fn release_claim() {
    let _ = BULK_STATE.compare_exchange(STATE_TESTING, STATE_UNTESTED, Ordering::AcqRel, Ordering::Acquire);
}

#[cfg(test)]
mod tests;
