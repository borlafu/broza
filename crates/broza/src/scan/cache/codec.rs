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
pub const STORE_VERSION: u8 = 1;
/// What the user can do about a cache Broza refuses to read.
pub const NO_CACHE_HINT: &str = "retry with --no-cache";
/// Bytes of the header: the magic plus the version byte.
const HEADER_LEN: usize = MAGIC.len() + 1;

/// Encode `records` into the bytes of a store file.
pub fn encode(records: &[DirRecord]) -> Result<Vec<u8>, BrozaError> {
    let body = postcard::to_stdvec(records).map_err(|error| cache_error(&format!("encode: {error}")))?;
    let mut bytes = Vec::with_capacity(HEADER_LEN + body.len());
    bytes.extend_from_slice(MAGIC);
    bytes.push(STORE_VERSION);
    bytes.extend_from_slice(&body);
    Ok(bytes)
}

/// Decode the bytes of a store file.
///
/// Every failure — wrong magic, unknown version, truncated or trailing bytes — is
/// a [`BrozaError::Cache`], so the CLI exits `9` and says what to do about it.
pub fn decode(bytes: &[u8]) -> Result<Vec<DirRecord>, BrozaError> {
    let (header, body) = bytes.split_at_checked(HEADER_LEN).ok_or_else(|| {
        cache_error(&format!("the store is {} bytes long, shorter than its header", bytes.len()))
    })?;
    if &header[..MAGIC.len()] != MAGIC {
        return Err(cache_error("the file does not start with the Broza cache magic"));
    }
    let version = header[MAGIC.len()];
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
    Ok(records)
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
            recorded_at: Timestamp::UNIX_EPOCH,
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

        let encoded = ok(encode(&records));
        let decoded = ok(decode(&encoded));

        assert_eq!(decoded, records);
    }

    #[test]
    fn the_file_starts_with_the_magic_and_the_version() {
        let encoded = ok(encode(&[record(1)]));

        assert_eq!(&encoded[..MAGIC.len()], MAGIC);
        assert_eq!(encoded[MAGIC.len()], STORE_VERSION);
    }

    #[test]
    fn an_empty_store_still_carries_a_header() {
        let encoded = ok(encode(&[]));

        assert!(encoded.len() > MAGIC.len());
        assert_eq!(ok(decode(&encoded)), Vec::new());
    }

    #[test]
    fn another_tools_file_is_refused_instead_of_being_guessed_at() {
        let message = expect_cache_error(b"NOPE\x01rubbish");

        assert!(message.contains(NO_CACHE_HINT), "{message}");
    }

    #[test]
    fn a_version_from_the_future_is_refused() {
        let mut bytes = ok(encode(&[record(1)]));
        bytes[MAGIC.len()] = STORE_VERSION + 1;

        let message = expect_cache_error(&bytes);

        assert!(message.contains(NO_CACHE_HINT), "{message}");
    }

    #[test]
    fn a_truncated_file_is_refused() {
        let bytes = ok(encode(&[record(1), record(2)]));

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
        let mut bytes = ok(encode(&[record(1)]));
        bytes.extend_from_slice(b"and then some");

        let _ = expect_cache_error(&bytes);
    }
}
