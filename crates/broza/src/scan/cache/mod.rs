//! The scan cache (`docs/cli-spec.md` §7, `docs/implementation-plan.md` §3.4).
//!
//! One store per volume, at `<cache-root>/v1/<volume>/dirs.bin`, keyed by the
//! `(device, inode, mtime)` of each directory. A directory whose key is unchanged
//! and whose record is younger than `cache-ttl` is served from the store and its
//! subtree is not walked at all — that is what turns a cold scan of ten seconds
//! into a warm one under a second.
//!
//! # What the cache does not promise
//!
//! A directory's `mtime` changes when an entry is added, removed, or renamed, but
//! **not** when a file already in it grows in place. A cached subtree can therefore
//! be stale by up to `cache-ttl`; the TTL is the bound, and `--no-cache` is the way
//! out. The store is never repaired: any header or decode failure is
//! [`BrozaError::Cache`](crate::BrozaError::Cache) (exit `9`) with the hint to
//! retry with `--no-cache`, because a cleanup tool that guesses at a corrupt file
//! is a cleanup tool that reports the wrong bytes.

pub mod codec;
pub mod key;
pub mod store;

pub use codec::{MAGIC, NO_CACHE_HINT, STORE_VERSION};
pub use key::{CacheKey, DirRecord};
pub use store::{BSD_ID_KEY_CODE, CacheStore, STORE_FILE_NAME, store_path};
