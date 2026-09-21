//! Reading one `getattrlistbulk` buffer, with no side effects.
//!
//! The kernel packs the attributes it returned tightly, in the order of their
//! bits, with no padding and **no placeholder for one it left out** —
//! `FSOPT_PACK_INVAL_ATTRS` makes no difference here, which is why nothing in
//! this file assumes a fixed offset. Every entry announces what it carries in
//! its `returned` set, and [`layout`] walks that set to find each field.
//!
//! Measured on macOS 26 (APFS), and pinned by the tests at the bottom:
//!
//! - a regular file and a symlink carry the file attributes, so both are read
//!   straight out of the buffer;
//! - a directory carries none of them and is left to `lstat`;
//! - `ATTR_CMN_ERROR` comes back only when there is an error to report, and an
//!   entry that carries one is handed to `lstat` instead. It is not warned
//!   about on its own: a recovered error changes nothing the user can act on,
//!   and if the `lstat` fails too, *that* failure travels to the report as the
//!   entry's own warning;
//! - `ATTR_FILE_DATALENGTH` is `st_size` while `ATTR_FILE_TOTALSIZE` adds the
//!   resource fork, and `ATTR_FILE_ALLOCSIZE` is `st_blocks × 512` while
//!   `ATTR_FILE_DATAALLOCSIZE` leaves the resource fork out. Broza reports what
//!   `lstat` reports, so it asks for the first of each pair.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use crate::ports::EntryMetadata;

/// `ATTR_CMN_ERROR`, which libc does not declare.
pub(super) const ATTR_CMN_ERROR: libc::attrgroup_t = 0x2000_0000;
/// `ATTR_CMN_*` bits asked for.
pub(super) const COMMON_ATTRS: libc::attrgroup_t = libc::ATTR_CMN_RETURNED_ATTRS
    | libc::ATTR_CMN_NAME
    | libc::ATTR_CMN_DEVID
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_MODTIME
    | libc::ATTR_CMN_ACCTIME
    | libc::ATTR_CMN_FLAGS
    | libc::ATTR_CMN_FILEID
    | ATTR_CMN_ERROR;
/// `ATTR_FILE_*` bits asked for; only files and symlinks carry them.
pub(super) const FILE_ATTRS: libc::attrgroup_t =
    libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_ALLOCSIZE | libc::ATTR_FILE_DATALENGTH;
/// Common attributes without which an entry cannot be read from the buffer.
const NEEDED_COMMON: libc::attrgroup_t = COMMON_ATTRS & !ATTR_CMN_ERROR & !libc::ATTR_CMN_RETURNED_ATTRS;

/// Bytes before the first attribute: the entry length and the returned set.
const HEADER_LEN: usize = 24;
/// Offset of the entry length.
const OFF_LENGTH: usize = 0;
/// Offset of the common group inside the returned-attributes set.
const OFF_RETURNED_COMMON: usize = 4;
/// Offset of the file group inside the returned-attributes set.
const OFF_RETURNED_FILE: usize = 16;
/// Shortest entry that can be read at all: the header plus a name reference.
pub(super) const MIN_ENTRY_LEN: usize = HEADER_LEN + 8;

/// `VDIR`, a directory.
const VDIR: u32 = 2;
/// `VLNK`, a symbolic link.
const VLNK: u32 = 5;
/// `SF_DATALESS`: the contents live in the cloud, not on this disk.
const SF_DATALESS: u32 = 0x4000_0000;
/// Seconds after the epoch a plausible modification time stays below
/// (1 January 2100), used to notice a misread buffer.
const IMPLAUSIBLE_AFTER: i64 = 4_102_444_800;

/// One entry of a directory, as the buffer described it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedEntry {
    /// The entry's name, never a path.
    pub name: PathBuf,
    /// Its metadata, or `None` when the buffer did not carry enough of it and
    /// the caller has to `lstat` the entry instead.
    pub meta: Option<EntryMetadata>,
}

/// Where each requested attribute sits inside one entry.
#[derive(Debug, Default)]
struct Layout {
    /// The `attrreference_t` naming the entry.
    name: Option<usize>,
    /// Device id.
    devid: Option<usize>,
    /// Object type.
    object_type: Option<usize>,
    /// Modification time.
    modified: Option<usize>,
    /// Access time.
    accessed: Option<usize>,
    /// `st_flags`.
    flags: Option<usize>,
    /// Inode number.
    inode: Option<usize>,
    /// Per-entry error, present only when the kernel has one to report.
    error: Option<usize>,
    /// Link count.
    link_count: Option<usize>,
    /// Allocated size of every fork.
    allocated: Option<usize>,
    /// Length of the data fork.
    length: Option<usize>,
}

/// Parse `count` entries out of `buffer`.
///
/// `None` means the buffer is not what this file expects, which is the caller's
/// signal to stop using the bulk reader altogether.
pub(super) fn parse_batch(buffer: &[u8], count: usize) -> Option<Vec<ParsedEntry>> {
    let mut entries = Vec::with_capacity(count);
    let mut cursor = 0_usize;
    for _ in 0..count {
        let rest = buffer.get(cursor..)?;
        let length = read_u32(rest, OFF_LENGTH)? as usize;
        if length < MIN_ENTRY_LEN || length > rest.len() {
            return None;
        }
        entries.push(parse_entry(rest.get(..length)?)?);
        cursor = cursor.checked_add(length)?;
    }
    Some(entries)
}

/// Parse one entry.
pub(super) fn parse_entry(entry: &[u8]) -> Option<ParsedEntry> {
    let returned_common = read_u32(entry, OFF_RETURNED_COMMON)?;
    let returned_file = read_u32(entry, OFF_RETURNED_FILE)?;
    let layout = layout(returned_common, returned_file);
    let name = read_name(entry, layout.name?)?;
    let complete = returned_common & NEEDED_COMMON == NEEDED_COMMON
        && returned_file & FILE_ATTRS == FILE_ATTRS
        && layout.error.is_none_or(|at| read_u32(entry, at) == Some(0));
    if !complete {
        return Some(ParsedEntry { name, meta: None });
    }
    let meta = metadata_of(entry, &layout)?;
    if !is_plausible(&meta) {
        // Numbers that cannot be true mean the buffer was read wrong, but the
        // remedy is to state this entry rather than to throw the directory
        // away: `lstat` is always right, and a reader that never verifies an
        // entry never earns the caller's trust either.
        return Some(ParsedEntry { name, meta: None });
    }
    Some(ParsedEntry { name, meta: Some(meta) })
}

/// Where the attributes of this entry are, given what the kernel returned.
fn layout(returned_common: libc::attrgroup_t, returned_file: libc::attrgroup_t) -> Layout {
    let mut at = HEADER_LEN;
    let mut take = |present: bool, size: usize| {
        if !present {
            return None;
        }
        let offset = at;
        at += size;
        Some(offset)
    };
    // In the order of the bits, which is the order the kernel packs them.
    let name = take(returned_common & libc::ATTR_CMN_NAME != 0, 8);
    let devid = take(returned_common & libc::ATTR_CMN_DEVID != 0, 4);
    let object_type = take(returned_common & libc::ATTR_CMN_OBJTYPE != 0, 4);
    let modified = take(returned_common & libc::ATTR_CMN_MODTIME != 0, 16);
    let accessed = take(returned_common & libc::ATTR_CMN_ACCTIME != 0, 16);
    let flags = take(returned_common & libc::ATTR_CMN_FLAGS != 0, 4);
    let inode = take(returned_common & libc::ATTR_CMN_FILEID != 0, 8);
    let error = take(returned_common & ATTR_CMN_ERROR != 0, 4);
    let link_count = take(returned_file & libc::ATTR_FILE_LINKCOUNT != 0, 4);
    let allocated = take(returned_file & libc::ATTR_FILE_ALLOCSIZE != 0, 8);
    let length = take(returned_file & libc::ATTR_FILE_DATALENGTH != 0, 8);
    Layout {
        name,
        devid,
        object_type,
        modified,
        accessed,
        flags,
        inode,
        error,
        link_count,
        allocated,
        length,
    }
}

/// Build the metadata of an entry that carries every attribute.
fn metadata_of(entry: &[u8], layout: &Layout) -> Option<EntryMetadata> {
    let object_type = read_u32(entry, layout.object_type?)?;
    Some(EntryMetadata {
        device: device_of(read_u32(entry, layout.devid?)?),
        inode: read_u64(entry, layout.inode?)?,
        size_bytes: read_u64(entry, layout.length?)?,
        allocated_bytes: read_u64(entry, layout.allocated?)?,
        link_count: u64::from(read_u32(entry, layout.link_count?)?),
        is_dir: object_type == VDIR,
        is_symlink: object_type == VLNK,
        is_dataless: read_u32(entry, layout.flags?)? & SF_DATALESS != 0,
        modified: read_time(entry, layout.modified?),
        accessed: read_time(entry, layout.accessed?),
    })
}

/// A device id the way `std` widens it: `dev_t` is signed, and a negative one
/// sign-extends, so `StdFileOps` and this reader answer the same number.
#[expect(
    clippy::cast_sign_loss,
    reason = "matching std::os::unix::fs::MetadataExt::dev, which widens the signed st_dev this way"
)]
fn device_of(raw: u32) -> u64 {
    i64::from(raw.cast_signed()) as u64
}

/// `true` when what came back could really have come from this filesystem.
///
/// A buffer read at the wrong offset still parses; it just yields nonsense. An
/// inode of zero, no device, no links at all, or a modification time beyond the
/// year 2100 are the cheap signs of exactly that. Such an entry is handed to
/// `lstat` instead of being believed.
fn is_plausible(meta: &EntryMetadata) -> bool {
    let times_are_sane =
        meta.modified.is_none_or(|time| time.as_second() >= 0 && time.as_second() < IMPLAUSIBLE_AFTER);
    meta.inode != 0 && meta.device != 0 && meta.link_count >= 1 && times_are_sane
}

/// The name of an entry, from the `attrreference_t` at `offset`.
fn read_name(entry: &[u8], offset: usize) -> Option<PathBuf> {
    // The reference is relative to itself, and signed: the kernel may place the
    // name before the field as well as after it.
    let relative = read_i32(entry, offset)?;
    let length = read_u32(entry, offset + 4)? as usize;
    let start = offset.checked_add_signed(isize::try_from(relative).ok()?)?;
    let end = start.checked_add(length.checked_sub(1)?)?;
    let bytes = entry.get(start..end)?;
    if bytes.is_empty() || bytes.contains(&b'/') {
        return None;
    }
    Some(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

/// A `timespec` at `offset`, as a timestamp.
fn read_time(entry: &[u8], offset: usize) -> Option<jiff::Timestamp> {
    let seconds = read_i64(entry, offset)?;
    let nanoseconds = read_i64(entry, offset + 8)?;
    jiff::Timestamp::new(seconds, i32::try_from(nanoseconds).ok()?).ok()
}

/// A `u32` at `offset`.
fn read_u32(entry: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(entry.get(offset..offset + 4)?.try_into().ok()?))
}

/// An `i32` at `offset`.
fn read_i32(entry: &[u8], offset: usize) -> Option<i32> {
    Some(i32::from_ne_bytes(entry.get(offset..offset + 4)?.try_into().ok()?))
}

/// A `u64` at `offset`.
fn read_u64(entry: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(entry.get(offset..offset + 8)?.try_into().ok()?))
}

/// An `i64` at `offset`.
fn read_i64(entry: &[u8], offset: usize) -> Option<i64> {
    Some(i64::from_ne_bytes(entry.get(offset..offset + 8)?.try_into().ok()?))
}

#[cfg(test)]
mod tests;
