//! On-disk encoding of the scan cache: a magic header plus `postcard` records.
//!
//! `postcard` is a compact, self-describing-enough binary format for `serde`
//! types; the four-byte magic and the version byte in front of it are what turn
//! "this file is not ours" and "this file is from a newer Broza" into a clean
//! error instead of a plausible-looking wrong answer.

use crate::BrozaError;
use crate::scan::cache::key::DirRecord;

/// First bytes of every store file.
pub const MAGIC: &[u8; 4] = b"BRZC";
/// Layout version of the store; bumped whenever [`DirRecord`] changes shape.
///
/// Version 3 (ADR 0008): records carry their child directories (with their
/// device) and their big files, so a subtree can be rebuilt from the store.
/// Version 2 was the same without the child's device and never shipped; a
/// file in either older layout is replaced, not refused.
pub const STORE_VERSION: u8 = 3;
/// What the user can do about a cache Broza refuses to read.
pub const NO_CACHE_HINT: &str = "retry with --no-cache";
/// Bytes of the header: the magic plus the version byte.
const HEADER_LEN: usize = MAGIC.len() + 1;

/// Encode `records` into the bytes of a store file.
///
/// Takes references so the caller does not have to copy a whole store's worth
/// of records to hand them over; `postcard` writes a `&T` exactly as it writes
/// a `T`, so the file is the same either way.
pub fn encode(records: &[&DirRecord]) -> Result<Vec<u8>, BrozaError> {
    let body = postcard::to_stdvec(records).map_err(|error| cache_error(&format!("encode: {error}")))?;
    let mut bytes = Vec::with_capacity(HEADER_LEN + body.len());
    bytes.extend_from_slice(MAGIC);
    bytes.push(STORE_VERSION);
    bytes.extend_from_slice(&body);
    Ok(bytes)
}

/// What decoding a store file produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// The records, none when the file was written by an older Broza.
    pub records: Vec<DirRecord>,
    /// `true` when the file was an older layout: nothing in it is read, and
    /// the store is rewritten in this layout on the next save.
    pub outdated: bool,
}

/// Decode the bytes of a store file.
///
/// Every failure — wrong magic, a version from the future, truncated or trailing
/// bytes — is a [`BrozaError::Cache`], so the CLI exits `9` and says what to do
/// about it. A file from an older Broza is not a failure: the cache is a
/// convenience, and upgrading must not cost the user an error. It is
/// [`Decoded::outdated`] and replaced.
pub fn decode(bytes: &[u8]) -> Result<Decoded, BrozaError> {
    let (header, body) = bytes.split_at_checked(HEADER_LEN).ok_or_else(|| {
        cache_error(&format!("the store is {} bytes long, shorter than its header", bytes.len()))
    })?;
    if &header[..MAGIC.len()] != MAGIC {
        return Err(cache_error("the file does not start with the Broza cache magic"));
    }
    let version = header[MAGIC.len()];
    if version < STORE_VERSION {
        return Ok(Decoded { records: Vec::new(), outdated: true });
    }
    if version != STORE_VERSION {
        return Err(cache_error(&format!(
            "the store is version {version}, this Broza writes version {STORE_VERSION}"
        )));
    }
    let (records, rest) = postcard::take_from_bytes::<Vec<DirRecord>>(body)
        .map_err(|error| cache_error(&format!("decode: {error}")))?;
    if !rest.is_empty() {
        return Err(cache_error(&format!("{} bytes left over after the records", rest.len())));
    }
    Ok(Decoded { records, outdated: false })
}

/// A cache error carrying the hint the user needs.
fn cache_error(what: &str) -> BrozaError {
    BrozaError::Cache(format!("{what}; {NO_CACHE_HINT}"))
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::{MAGIC, NO_CACHE_HINT, STORE_VERSION, decode, encode};
    use crate::BrozaError;
    use crate::ExitCode;
    use crate::scan::cache::key::{CacheKey, DirRecord};

    fn record(inode: u64) -> DirRecord {
        DirRecord {
            key: CacheKey { device: 1, inode, mtime_ns: 1_700_000_000_000_000_000 },
            size_bytes: 4096,
            allocated_bytes: 8192,
            file_count: 2,
            dir_count: 3,
            dataless_count: 0,
            largest_item_bytes: 4096,
            has_hard_links: false,
            has_truncation: false,
            recorded_at: Timestamp::UNIX_EPOCH,
            child_dirs: Vec::new(),
            files: Vec::new(),
        }
    }

    /// Unwrap what the test expects to succeed, through one shared panic site.
    fn ok<T>(result: Result<T, BrozaError>) -> T {
        result.unwrap_or_else(|error| panic!("unexpected failure: {error}"))
    }

    fn expect_cache_error(bytes: &[u8]) -> String {
        match decode(bytes) {
            Err(error @ BrozaError::Cache(_)) => {
                assert_eq!(ExitCode::from(&error), ExitCode::CacheError);
                error.to_string()
            }
            other => panic!("expected a cache error, got {other:?}"),
        }
    }

    #[test]
    fn records_survive_a_round_trip() {
        let records = vec![record(1), record(2)];

        let borrowed: Vec<&DirRecord> = records.iter().collect();
        let encoded = ok(encode(&borrowed));
        let decoded = ok(decode(&encoded));

        assert_eq!(decoded.records, records);
        assert!(!decoded.outdated);
    }

    #[test]
    fn the_file_starts_with_the_magic_and_the_version() {
        let encoded = ok(encode(&[&record(1)]));

        assert_eq!(&encoded[..MAGIC.len()], MAGIC);
        assert_eq!(encoded[MAGIC.len()], STORE_VERSION);
    }

    #[test]
    fn an_empty_store_still_carries_a_header() {
        let encoded = ok(encode(&[]));

        assert!(encoded.len() > MAGIC.len());
        assert_eq!(ok(decode(&encoded)).records, Vec::new());
    }

    #[test]
    fn another_tools_file_is_refused_instead_of_being_guessed_at() {
        let message = expect_cache_error(b"NOPE\x01rubbish");

        assert!(message.contains(NO_CACHE_HINT), "{message}");
    }

    #[test]
    fn a_version_from_the_future_is_refused() {
        let mut bytes = ok(encode(&[&record(1)]));
        bytes[MAGIC.len()] = STORE_VERSION + 1;

        let message = expect_cache_error(&bytes);

        assert!(message.contains(NO_CACHE_HINT), "{message}");
    }

    /// The layout-2 record, as that version wrote it: a child without a device.
    #[derive(serde::Serialize)]
    struct LegacyChild {
        name: Vec<u8>,
        inode: u64,
        mtime_ns: Option<i128>,
    }

    #[derive(serde::Serialize)]
    struct LegacyRecord {
        key: CacheKey,
        size_bytes: u64,
        allocated_bytes: u64,
        file_count: u64,
        dir_count: u64,
        dataless_count: u64,
        largest_item_bytes: u64,
        has_hard_links: bool,
        has_truncation: bool,
        recorded_at: Timestamp,
        child_dirs: Vec<LegacyChild>,
        files: Vec<()>,
    }

    #[test]
    fn a_store_from_an_older_broza_is_replaced_not_refused() {
        let legacy = LegacyRecord {
            key: CacheKey { device: 1, inode: 1, mtime_ns: 1 },
            size_bytes: 1,
            allocated_bytes: 1,
            file_count: 1,
            dir_count: 1,
            dataless_count: 0,
            largest_item_bytes: 1,
            has_hard_links: false,
            has_truncation: false,
            recorded_at: Timestamp::UNIX_EPOCH,
            child_dirs: vec![LegacyChild { name: b"x".to_vec(), inode: 2, mtime_ns: Some(1) }],
            files: Vec::new(),
        };
        let body = postcard::to_stdvec(&vec![legacy]).unwrap_or_else(|e| panic!("{e}"));
        let mut bytes = MAGIC.to_vec();
        bytes.push(STORE_VERSION - 1);
        bytes.extend_from_slice(&body);

        let decoded = ok(decode(&bytes));

        assert!(decoded.outdated);
        assert!(decoded.records.is_empty(), "nothing of the old layout is read, let alone misread");
    }

    #[test]
    fn a_truncated_file_is_refused() {
        let bytes = ok(encode(&[&record(1), &record(2)]));

        let message = expect_cache_error(&bytes[..bytes.len() - 3]);

        assert!(message.contains(NO_CACHE_HINT), "{message}");
    }

    #[test]
    fn a_file_shorter_than_the_header_is_refused() {
        let _ = expect_cache_error(b"BR");
        let _ = expect_cache_error(&[]);
    }

    #[test]
    fn trailing_rubbish_after_the_records_is_refused() {
        let mut bytes = ok(encode(&[&record(1)]));
        bytes.extend_from_slice(b"and then some");

        let _ = expect_cache_error(&bytes);
    }
}
