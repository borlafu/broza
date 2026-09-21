//! What the parser must make of a buffer, byte by byte.

use std::path::Path;

use super::{ATTR_CMN_ERROR, COMMON_ATTRS, FILE_ATTRS, MIN_ENTRY_LEN, device_of, parse_batch, parse_entry};

/// Inode used by the entries built here.
const INODE: u64 = 42;
/// Device used by the entries built here.
const DEVICE: u32 = 16_777_232;
/// Data-fork length of the entry built here.
const LENGTH: u64 = 1234;
/// Allocated size of the entry built here.
const ALLOCATED: u64 = 8192;

/// A `u32` that a `usize` fits into, or a test failure.
fn small(value: usize) -> u32 {
    u32::try_from(value).unwrap_or_else(|error| panic!("{value} does not fit: {error}"))
}

/// One entry laid out the way the kernel lays one out.
///
/// `returned_common` and `returned_file` decide which fields are present, so a
/// test can build the buffer a directory or a broken entry would produce.
fn entry(returned_common: u32, returned_file: u32, name: &[u8], inode: u64, error: u32) -> Vec<u8> {
    let mut body = Vec::new();
    let mut push_if = |present: bool, bytes: &[u8]| {
        if present {
            body.extend_from_slice(bytes);
        }
    };
    // The name reference comes first; its offset is filled in below.
    push_if(returned_common & libc::ATTR_CMN_NAME != 0, &[0_u8; 8]);
    push_if(returned_common & libc::ATTR_CMN_DEVID != 0, &DEVICE.to_ne_bytes());
    push_if(returned_common & libc::ATTR_CMN_OBJTYPE != 0, &1_u32.to_ne_bytes());
    push_if(returned_common & libc::ATTR_CMN_MODTIME != 0, &[0_u8; 16]);
    push_if(returned_common & libc::ATTR_CMN_ACCTIME != 0, &[0_u8; 16]);
    push_if(returned_common & libc::ATTR_CMN_FLAGS != 0, &0_u32.to_ne_bytes());
    push_if(returned_common & libc::ATTR_CMN_FILEID != 0, &inode.to_ne_bytes());
    push_if(returned_common & ATTR_CMN_ERROR != 0, &error.to_ne_bytes());
    push_if(returned_file & libc::ATTR_FILE_LINKCOUNT != 0, &1_u32.to_ne_bytes());
    push_if(returned_file & libc::ATTR_FILE_ALLOCSIZE != 0, &ALLOCATED.to_ne_bytes());
    push_if(returned_file & libc::ATTR_FILE_DATALENGTH != 0, &LENGTH.to_ne_bytes());

    let mut bytes = Vec::new();
    let length = super::HEADER_LEN + body.len() + name.len() + 1;
    bytes.extend_from_slice(&small(length).to_ne_bytes());
    bytes.extend_from_slice(&returned_common.to_ne_bytes());
    bytes.extend_from_slice(&0_u32.to_ne_bytes()); // volattr
    bytes.extend_from_slice(&0_u32.to_ne_bytes()); // dirattr
    bytes.extend_from_slice(&returned_file.to_ne_bytes());
    bytes.extend_from_slice(&0_u32.to_ne_bytes()); // forkattr
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(name);
    bytes.push(0);
    // The name sits after every attribute, and the reference is relative to
    // itself — the first attribute, at the end of the header.
    let name_offset = small(body.len());
    bytes[super::HEADER_LEN..super::HEADER_LEN + 4].copy_from_slice(&name_offset.to_ne_bytes());
    bytes[super::HEADER_LEN + 4..super::HEADER_LEN + 8].copy_from_slice(&small(name.len() + 1).to_ne_bytes());
    bytes
}

/// An entry with everything the reader asks for, error field clear.
fn whole_entry() -> Vec<u8> {
    entry(COMMON_ATTRS & !ATTR_CMN_ERROR, FILE_ATTRS, b"file", INODE, 0)
}

#[test]
fn an_entry_with_every_attribute_is_read_out_of_the_buffer() {
    let parsed = parse_entry(&whole_entry()).unwrap_or_else(|| panic!("the whole set must parse"));

    assert_eq!(parsed.name, Path::new("file"));
    let meta = parsed.meta.unwrap_or_else(|| panic!("the whole set needs no fallback"));
    assert_eq!(meta.inode, INODE);
    assert_eq!(meta.size_bytes, LENGTH, "the data fork, as lstat reports it");
    assert_eq!(meta.allocated_bytes, ALLOCATED, "every fork, as st_blocks counts them");
    assert_eq!(meta.link_count, 1);
    assert_eq!(meta.device, u64::from(DEVICE));
    assert!(!meta.is_dir);
    assert!(!meta.is_dataless);
}

#[test]
fn an_entry_carrying_no_file_attributes_is_left_to_lstat() {
    // This is what a directory looks like: the common set, and nothing else.
    let directory = entry(COMMON_ATTRS & !ATTR_CMN_ERROR, 0, b"sub", INODE, 0);

    let parsed = parse_entry(&directory).unwrap_or_else(|| panic!("a directory still parses"));

    assert_eq!(parsed.name, Path::new("sub"));
    assert_eq!(parsed.meta, None, "the caller has to state it");
}

#[test]
fn an_entry_the_kernel_reports_an_error_for_is_left_to_lstat() {
    let broken = entry(COMMON_ATTRS, FILE_ATTRS, b"file", INODE, libc::EIO as u32);

    let parsed = parse_entry(&broken).unwrap_or_else(|| panic!("a reported error still parses"));

    assert_eq!(parsed.meta, None);
}

#[test]
fn an_entry_whose_error_field_is_clear_is_still_read_from_the_buffer() {
    let clean = entry(COMMON_ATTRS, FILE_ATTRS, b"file", INODE, 0);

    let parsed = parse_entry(&clean).unwrap_or_else(|| panic!("no error means no fallback"));

    assert!(parsed.meta.is_some());
}

#[test]
fn an_entry_that_does_not_even_carry_a_name_is_not_read() {
    let nameless = entry(COMMON_ATTRS & !libc::ATTR_CMN_NAME & !ATTR_CMN_ERROR, FILE_ATTRS, b"f", INODE, 0);

    assert!(parse_entry(&nameless).is_none());
}

#[test]
fn an_entry_whose_numbers_cannot_be_true_is_left_to_lstat() {
    let no_inode = entry(COMMON_ATTRS & !ATTR_CMN_ERROR, FILE_ATTRS, b"file", 0, 0);

    let parsed = parse_entry(&no_inode).unwrap_or_else(|| panic!("the entry still has a name"));

    // Nonsense in the buffer is this entry's problem: `lstat` answers for it,
    // and the reader stays untrusted because nothing was ever confirmed.
    assert_eq!(parsed.meta, None);
}

#[test]
fn a_name_that_is_not_a_name_is_not_read() {
    let slashed = entry(COMMON_ATTRS & !ATTR_CMN_ERROR, FILE_ATTRS, b"a/b", INODE, 0);

    assert!(parse_entry(&slashed).is_none());
}

#[test]
fn a_batch_that_claims_more_bytes_than_it_holds_is_not_read() {
    let mut short = whole_entry();
    short.truncate(MIN_ENTRY_LEN + 1);

    assert!(parse_batch(&short, 1).is_none());
}

#[test]
fn a_batch_of_two_entries_comes_back_in_order() {
    let mut buffer = whole_entry();
    buffer.extend(entry(COMMON_ATTRS & !ATTR_CMN_ERROR, 0, b"sub", INODE + 1, 0));

    let parsed = parse_batch(&buffer, 2).unwrap_or_else(|| panic!("two good entries must parse"));

    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].name, Path::new("file"));
    assert_eq!(parsed[1].name, Path::new("sub"));
}

#[test]
fn a_device_id_widens_the_way_std_widens_it() {
    assert_eq!(device_of(DEVICE), u64::from(DEVICE));
    // `dev_t` is signed: a high bit means a negative id, and `std` sign-extends
    // it. Zero-extending here would disagree with `lstat` on such a volume.
    assert_eq!(device_of(0xFFFF_FFFF), u64::MAX);
}
