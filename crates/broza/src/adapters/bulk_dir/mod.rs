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

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::BrozaError;
use crate::adapters::dir_fd::{metadata_in, open_directory, retrying};
use crate::ports::{DirListing, EntryMetadata};
use parse::ParsedEntry;

/// What came back from trying to read one directory in bulk.
enum Collected {
    /// The directory was read.
    Entries(Vec<ParsedEntry>),
    /// This directory could not be opened or read — no permission, gone,
    /// something else. That says nothing about the reader itself.
    Unavailable,
    /// The kernel cannot do this here at all, or the buffer did not read the
    /// way [`parse`] expects. Either way, stop using the reader.
    Unsupported,
}

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
    let dir = open_directory(path).ok()?;
    read_dir_in(&dir, path)
}

/// [`read_dir_with_attributes`] over an already open directory: nothing here
/// hands `path` to the kernel, so the depth of the tree does not matter.
pub(crate) fn read_dir_in(dir: &File, path: &Path) -> Option<DirListing> {
    match BULK_STATE.load(Ordering::Acquire) {
        STATE_TRUSTED => match collect(dir) {
            Collected::Entries(parsed) => Some(resolve(dir, path, parsed)),
            Collected::Unavailable => None,
            Collected::Unsupported => refuse(),
        },
        STATE_UNTESTED => try_first_directory(dir, path),
        // Refused for good, or somebody else is deciding right now.
        _ => None,
    }
}

/// Read the first directory and decide whether the reader can be trusted.
///
/// Only the thread that wins the claim does this; the rest fall back, because
/// waiting for it would cost more than the `lstat`s they are avoiding.
fn try_first_directory(dir: &File, path: &Path) -> Option<DirListing> {
    if BULK_STATE
        .compare_exchange(STATE_UNTESTED, STATE_TESTING, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return None;
    }
    let parsed = match collect(dir) {
        Collected::Entries(parsed) => parsed,
        // One directory Broza may not read says nothing about the kernel.
        Collected::Unavailable => {
            release_claim();
            return None;
        }
        Collected::Unsupported => return refuse(),
    };
    let Some(verified) = agrees_with_lstat(dir, path, &parsed) else {
        return refuse();
    };
    let from_buffer = parsed.iter().filter(|entry| entry.meta.is_some()).count();
    if from_buffer > 0 && verified == 0 {
        // The buffer described entries and not one of them could be confirmed:
        // hand the answer back rather than trusting it, and let the next
        // directory decide.
        release_claim();
        return None;
    }
    // A directory holding nothing the buffer itself described proves nothing —
    // every entry was stated the plain way — so leave the question open.
    let settled = if verified > 0 { STATE_TRUSTED } else { STATE_UNTESTED };
    let _ = BULK_STATE.compare_exchange(STATE_TESTING, settled, Ordering::AcqRel, Ordering::Acquire);
    Some(resolve(dir, path, parsed))
}

/// Every entry of the open directory as the buffer described it.
fn collect(dir: &File) -> Collected {
    let mut request = request_list();
    let mut entries = Vec::new();
    loop {
        let read = BUFFER.with(|buffer| {
            let mut buffer = buffer.borrow_mut();
            let count = call_bulk(dir, &mut request, &mut buffer);
            let parsed = usize::try_from(count)
                .ok()
                .map_or(Some(Vec::new()), |count| parse::parse_batch(&buffer, count));
            (count, parsed)
        });
        match read {
            (count, _) if count < 0 => return failed_call(),
            (0, _) => return Collected::Entries(entries),
            (_, None) => return Collected::Unsupported,
            (_, Some(parsed)) => entries.extend(parsed),
        }
    }
}

/// One `getattrlistbulk` call; negative means the kernel refused.
fn call_bulk(dir: &File, request: &mut libc::attrlist, buffer: &mut [u8]) -> i32 {
    // SAFETY: `request` is the fixed-size struct libc declares and `buffer` is
    // a live allocation of `buffer.len()` bytes; the kernel writes no more than
    // that. The descriptor belongs to a `File` that outlives the call. An
    // interrupted call is asked again, as `std` does for its own calls.
    retrying(|| unsafe {
        libc::getattrlistbulk(
            dir.as_raw_fd(),
            std::ptr::from_mut(request).cast(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            u64::from(libc::FSOPT_ATTR_CMN_EXTENDED),
        )
    })
    .unwrap_or(-1)
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
        // The extended common attributes, which `FSOPT_ATTR_CMN_EXTENDED`
        // makes this slot mean.
        forkattr: parse::EXTENDED_ATTRS,
    }
}

/// Turn parsed entries into a listing, stating whatever the buffer left out —
/// by name inside the open directory, never by path.
fn resolve(dir: &File, parent: &Path, parsed: Vec<ParsedEntry>) -> DirListing {
    parsed
        .into_iter()
        .map(|entry| {
            let path = parent.join(&entry.name);
            let meta = match entry.meta {
                Some(meta) => Ok(meta),
                None => stat(dir, &entry.name, &path),
            };
            (path, meta)
        })
        .collect()
}

/// Metadata of the entry `name` inside `dir` the plain way (`fstatat`).
fn stat(dir: &File, name: &Path, path: &Path) -> Result<EntryMetadata, BrozaError> {
    metadata_in(dir, name.as_os_str(), path)
}

/// How many entries the buffer described and `lstat` confirms; `None` on any
/// disagreement.
///
/// Entries the parser left to `lstat` are skipped: comparing them against the
/// call they came from proves nothing. An entry that has vanished since the
/// listing is skipped too — a race is not a reason to distrust the kernel.
/// Neither is a file that changed between the two calls: the plain reader is
/// itself two calls (`lstat`, then `getattrlist` for the clone id), so a
/// disagreement is stated a second time and only counts when it holds.
fn agrees_with_lstat(dir: &File, parent: &Path, parsed: &[ParsedEntry]) -> Option<usize> {
    let mut verified = 0_usize;
    for entry in parsed {
        let Some(meta) = &entry.meta else { continue };
        let path = parent.join(&entry.name);
        let Ok(stated) = stat(dir, &entry.name, &path) else { continue };
        if &stated != meta {
            let Ok(again) = stat(dir, &entry.name, &path) else { continue };
            if &again != meta && again == stated {
                return None;
            }
            continue;
        }
        verified += 1;
    }
    Some(verified)
}

/// Read the errno of a failed call: a kernel that cannot do this at all is a
/// different thing from one directory that went wrong.
///
/// `ENOTSUP` means the filesystem does not implement the call, and no other
/// directory on it will either.
fn failed_call() -> Collected {
    if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOTSUP) {
        return Collected::Unsupported;
    }
    Collected::Unavailable
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
