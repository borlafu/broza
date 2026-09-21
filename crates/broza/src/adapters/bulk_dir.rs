//! One directory, one syscall: `getattrlistbulk` instead of `readdir` + `lstat`.
//!
//! A scan of a home directory looks at millions of entries, and the plain pair
//! costs two syscalls each — that is where the seconds of a cold scan go
//! (`docs/cli-spec.md` §7). `getattrlistbulk(2)` returns a whole directory's
//! entries *with* their attributes in one call, which is what every fast
//! du-style tool on macOS uses.
//!
//! # Trusting it
//!
//! The kernel packs the requested attributes in a fixed order, and this file
//! reads them back by offset. An offset that is wrong by four bytes would not
//! crash — it would report wrong sizes, which is worse. So the reader:
//!
//! 1. checks, per entry, that the kernel returned exactly the attributes asked
//!    for and that what came back is plausible ([`is_plausible`]);
//! 2. cross-checks the first directory it ever reads against `lstat`, entry by
//!    entry, and switches the whole process back to the plain pair for good if
//!    anything disagrees ([`BULK_STATE`]);
//! 3. falls back for any single entry whose file attributes the kernel did not
//!    return (directories and symlinks, which are a small minority).
//!
//! `crates/broza/tests/fakes_behave_like_std.rs` pins the two readers together.

#![allow(unsafe_code)]

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};

use crate::ports::EntryMetadata;

/// Bytes handed to the kernel per call; big enough for a large directory.
const BUFFER_BYTES: usize = 256 * 1024;
/// Length of an entry that carries every attribute this file asks for.
///
/// The kernel packs attributes tightly, in the order of their bits, with no
/// padding: the inode lands right after the flags on a four-byte boundary, and
/// an attribute that does not apply is left out rather than zero-filled. So an
/// entry is only read by these offsets when the kernel says it returned the
/// whole set; everything else is stated the plain way.
const FULL_ENTRY_LEN: usize = 104;
/// Shortest entry that can still be read at all: length, returned set, name.
const MIN_ENTRY_LEN: usize = 32;
/// Offset of the entry length.
const OFF_LENGTH: usize = 0;
/// Offset of the returned-attributes set.
const OFF_RETURNED: usize = 4;
/// Offset of the file group inside the returned-attributes set.
const OFF_RETURNED_FILE: usize = 12;
/// Offset of the name reference.
const OFF_NAME_REF: usize = 24;
/// Offset of the device id.
const OFF_DEVID: usize = 32;
/// Offset of the object type.
const OFF_OBJTYPE: usize = 36;
/// Offset of the modification time.
const OFF_MODTIME: usize = 40;
/// Offset of the access time.
const OFF_ACCTIME: usize = 56;
/// Offset of the `st_flags` bits.
const OFF_FLAGS: usize = 72;
/// Offset of the inode number.
const OFF_FILEID: usize = 76;
/// Offset of the link count.
const OFF_LINKCOUNT: usize = 84;
/// Offset of the apparent size.
const OFF_TOTALSIZE: usize = 88;
/// Offset of the allocated size.
const OFF_ALLOCSIZE: usize = 96;

/// `ATTR_CMN_*` bits requested; the order here is the order in the buffer.
const COMMON_ATTRS: libc::attrgroup_t = libc::ATTR_CMN_RETURNED_ATTRS
    | libc::ATTR_CMN_NAME
    | libc::ATTR_CMN_DEVID
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_MODTIME
    | libc::ATTR_CMN_ACCTIME
    | libc::ATTR_CMN_FLAGS
    | libc::ATTR_CMN_FILEID;
/// `ATTR_FILE_*` bits requested; only regular files have them.
const FILE_ATTRS: libc::attrgroup_t =
    libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_TOTALSIZE | libc::ATTR_FILE_ALLOCSIZE;

/// `VDIR`, a directory.
const VDIR: u32 = 2;
/// `VLNK`, a symbolic link.
const VLNK: u32 = 5;
/// `SF_DATALESS`: the contents live in the cloud, not on this disk.
const SF_DATALESS: u32 = 0x4000_0000;
/// Seconds after the epoch a plausible modification time stays below
/// (1 January 2100), used to notice a misread buffer.
const IMPLAUSIBLE_AFTER: i64 = 4_102_444_800;

/// Whether the bulk reader may be used: untested, trusted, or given up on.
static BULK_STATE: AtomicU8 = AtomicU8::new(STATE_UNTESTED);
/// Nothing has been read yet; the next directory is cross-checked.
const STATE_UNTESTED: u8 = 0;
/// The cross-check passed; entries are trusted.
const STATE_TRUSTED: u8 = 1;
/// Something did not add up; the plain pair is used from now on.
const STATE_REFUSED: u8 = 2;

/// Read a whole directory with its attributes, or `None` to use the plain pair.
///
/// `None` means "this did not work here": an unsupported filesystem, an error
/// from the kernel, or an entry that failed the plausibility checks.
pub(crate) fn read_dir_with_attributes(path: &Path) -> Option<Vec<(PathBuf, EntryMetadata)>> {
    if BULK_STATE.load(Ordering::Relaxed) == STATE_REFUSED {
        return None;
    }
    let entries = collect(path)?;
    if BULK_STATE.load(Ordering::Relaxed) == STATE_UNTESTED {
        return cross_check(entries);
    }
    Some(entries)
}

/// Read every entry of `path` through `getattrlistbulk`.
fn collect(path: &Path) -> Option<Vec<(PathBuf, EntryMetadata)>> {
    let dir = File::open(path).ok()?;
    let mut request = request_list();
    let mut buffer = vec![0_u8; BUFFER_BYTES];
    let mut entries = Vec::new();
    loop {
        // SAFETY: `request` and `buffer` are live, owned, correctly sized
        // allocations; the kernel writes at most `buffer.len()` bytes into the
        // buffer and reads `attrlist` as the fixed-size struct libc declares.
        // The descriptor comes from an open `File` that outlives the call.
        let count = unsafe {
            libc::getattrlistbulk(
                dir.as_raw_fd(),
                std::ptr::from_mut(&mut request).cast(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                u64::from(libc::FSOPT_PACK_INVAL_ATTRS),
            )
        };
        if count < 0 {
            return None;
        }
        if count == 0 {
            return Some(entries);
        }
        entries = parse_batch(&buffer, count, path, entries)?;
    }
}

/// The attribute list handed to the kernel.
fn request_list() -> libc::attrlist {
    libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: COMMON_ATTRS,
        volattr: 0,
        dirattr: 0,
        fileattr: FILE_ATTRS,
        forkattr: 0,
    }
}

/// Parse `count` entries out of `buffer`, appending them to `entries`.
fn parse_batch(
    buffer: &[u8],
    count: i32,
    parent: &Path,
    mut entries: Vec<(PathBuf, EntryMetadata)>,
) -> Option<Vec<(PathBuf, EntryMetadata)>> {
    let mut cursor = 0_usize;
    for _ in 0..count {
        let rest = buffer.get(cursor..)?;
        let length = read_u32(rest, OFF_LENGTH)? as usize;
        if length < MIN_ENTRY_LEN || length > rest.len() {
            return refuse();
        }
        let entry = rest.get(..length)?;
        let (name, meta) = parse_entry(entry, parent)?;
        entries.push((parent.join(name), meta));
        cursor = cursor.checked_add(length)?;
    }
    Some(entries)
}

/// Turn one entry into a name and the metadata of a `lstat`.
///
/// Only an entry carrying every requested attribute is read from the buffer; a
/// directory or a symlink carries no file attributes, so it is stated the plain
/// way. Those are the minority — a home directory is mostly regular files.
fn parse_entry(entry: &[u8], parent: &Path) -> Option<(PathBuf, EntryMetadata)> {
    let returned_common = read_u32(entry, OFF_RETURNED)?;
    if returned_common & libc::ATTR_CMN_NAME == 0 {
        return refuse();
    }
    let name = read_name(entry)?;
    let has_everything = returned_common == COMMON_ATTRS
        && read_u32(entry, OFF_RETURNED + OFF_RETURNED_FILE)? == FILE_ATTRS
        && entry.len() >= FULL_ENTRY_LEN;
    if !has_everything {
        let stated = stat(&parent.join(&name))?;
        return Some((name, stated));
    }
    let object_type = read_u32(entry, OFF_OBJTYPE)?;
    let meta = EntryMetadata {
        device: u64::from(read_u32(entry, OFF_DEVID)?),
        inode: read_u64(entry, OFF_FILEID)?,
        size_bytes: read_u64(entry, OFF_TOTALSIZE)?,
        allocated_bytes: read_u64(entry, OFF_ALLOCSIZE)?,
        link_count: u64::from(read_u32(entry, OFF_LINKCOUNT)?),
        is_dir: object_type == VDIR,
        is_symlink: object_type == VLNK,
        is_dataless: read_u32(entry, OFF_FLAGS)? & SF_DATALESS != 0,
        modified: read_time(entry, OFF_MODTIME),
        accessed: read_time(entry, OFF_ACCTIME),
    };
    if !is_plausible(&meta) {
        return refuse();
    }
    Some((name, meta))
}

/// `true` when what came back could really have come from this filesystem.
///
/// A buffer read at the wrong offset still parses; it just yields nonsense.
/// An inode of zero, no device, a modification time beyond the year 2100 or a
/// file with no links at all are the cheap signs of exactly that.
fn is_plausible(meta: &EntryMetadata) -> bool {
    let times_are_sane =
        meta.modified.is_none_or(|time| time.as_second() >= 0 && time.as_second() < IMPLAUSIBLE_AFTER);
    meta.inode != 0 && meta.device != 0 && meta.link_count >= 1 && times_are_sane
}

/// Metadata of `path` the plain way, through the adapter everything else uses.
fn stat(path: &Path) -> Option<EntryMetadata> {
    crate::ports::FileOps::metadata(&crate::adapters::StdFileOps, path).ok()
}

/// Give up on the bulk reader for the rest of the process.
fn refuse<T>() -> Option<T> {
    BULK_STATE.store(STATE_REFUSED, Ordering::Relaxed);
    None
}

/// Compare a whole directory against `lstat` before trusting the reader.
fn cross_check(entries: Vec<(PathBuf, EntryMetadata)>) -> Option<Vec<(PathBuf, EntryMetadata)>> {
    let agrees = entries.iter().all(|(path, meta)| stat(path).is_some_and(|expected| &expected == meta));
    if !agrees {
        return refuse();
    }
    BULK_STATE.store(STATE_TRUSTED, Ordering::Relaxed);
    Some(entries)
}

/// The name of an entry, from its `attrreference_t`.
fn read_name(entry: &[u8]) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let offset = read_u32(entry, OFF_NAME_REF)? as usize;
    let length = read_u32(entry, OFF_NAME_REF + 4)? as usize;
    let start = OFF_NAME_REF.checked_add(offset)?;
    let end = start.checked_add(length.checked_sub(1)?)?;
    let bytes = entry.get(start..end)?;
    if bytes.is_empty() || bytes.contains(&b'/') {
        return refuse();
    }
    Some(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

/// A `timespec` at `offset`, as a timestamp.
fn read_time(entry: &[u8], offset: usize) -> Option<jiff::Timestamp> {
    let seconds = i64::from_ne_bytes(entry.get(offset..offset + 8)?.try_into().ok()?);
    let nanoseconds = i64::from_ne_bytes(entry.get(offset + 8..offset + 16)?.try_into().ok()?);
    jiff::Timestamp::new(seconds, i32::try_from(nanoseconds).ok()?).ok()
}

/// A `u32` at `offset`.
fn read_u32(entry: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(entry.get(offset..offset + 4)?.try_into().ok()?))
}

/// A `u64` at `offset`.
fn read_u64(entry: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(entry.get(offset..offset + 8)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{BULK_STATE, Ordering, STATE_REFUSED, read_dir_with_attributes};

    /// Restore the reader's state so one test cannot disable another.
    struct KeepState(u8);

    impl Drop for KeepState {
        fn drop(&mut self) {
            BULK_STATE.store(self.0, Ordering::Relaxed);
        }
    }

    fn keep_state() -> KeepState {
        KeepState(BULK_STATE.load(Ordering::Relaxed))
    }

    #[test]
    fn a_real_directory_comes_back_with_the_same_entries_as_read_dir() {
        let _state = keep_state();
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(dir.path().join("file"), vec![0_u8; 1234]).unwrap_or_else(|e| panic!("{e}"));
        std::fs::create_dir(dir.path().join("sub")).unwrap_or_else(|e| panic!("{e}"));

        let Some(entries) = read_dir_with_attributes(dir.path()) else {
            eprintln!("skipped: getattrlistbulk is not usable here");
            return;
        };

        let mut names: Vec<String> = entries
            .iter()
            .filter_map(|(path, _)| path.file_name().map(|name| name.to_string_lossy().into_owned()))
            .collect();
        names.sort();
        assert_eq!(names, vec!["file".to_owned(), "sub".to_owned()]);
        let file = entries
            .iter()
            .find(|(path, _)| path.ends_with("file"))
            .unwrap_or_else(|| panic!("no file entry"));
        assert_eq!(file.1.size_bytes, 1234);
        assert!(!file.1.is_dir);
        assert!(!file.1.is_dataless);
        assert!(entries.iter().any(|(_, meta)| meta.is_dir));
    }

    #[test]
    fn a_reader_that_was_given_up_on_answers_nothing() {
        let _state = keep_state();
        BULK_STATE.store(STATE_REFUSED, Ordering::Relaxed);

        assert!(read_dir_with_attributes(Path::new("/")).is_none());
    }

    #[test]
    fn a_path_that_is_not_a_directory_is_not_read() {
        let _state = keep_state();
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let file = dir.path().join("file");
        std::fs::write(&file, b"x").unwrap_or_else(|e| panic!("{e}"));

        assert!(read_dir_with_attributes(&file).is_none());
    }
}
