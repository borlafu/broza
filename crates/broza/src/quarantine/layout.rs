//! Paths inside the quarantine store, and the identifier of a new session.
//!
//! Pure functions: nothing here touches the filesystem, so the layout described by
//! [ADR 0004] can be asserted without a tree.
//!
//! ```text
//! <root>/
//!   cln_20260921103608_a1b2/
//!     manifest.json
//!     items/
//!       0001/<basename>
//!       0002/<basename>
//! ```
//!
//! [ADR 0004]: https://github.com/borlafu/broza/blob/main/docs/adr/0004-quarantine-same-volume-and-quarantine-command.md

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::BrozaError;
use crate::model::{EntryId, SessionId};
use crate::ports::Clock;

/// Name of the manifest inside a session directory.
pub const MANIFEST_FILE: &str = "manifest.json";
/// Name of the directory holding the moved items of a session.
pub const ITEMS_DIR: &str = "items";
/// Width of the zero-padded sequence number of an item directory.
const SEQUENCE_WIDTH: usize = 4;
/// Sequence number of the first item of a session.
pub const FIRST_SEQUENCE: u32 = 1;
/// `strftime` pattern of the timestamp part of a session identifier.
const SESSION_STAMP_PATTERN: &str = "%Y%m%d%H%M%S";
/// Prefix of every session identifier.
const SESSION_PREFIX: &str = "cln";
/// Number of distinct suffixes derived from the sub-second part of the clock.
const SUFFIX_SPACE: u32 = 0x1_0000;

/// Root of the quarantine store for the configured quarantine directory.
///
/// `configured` is `quarantine-path` after [`Config::quarantine_dir`] expanded a
/// leading `~`. An identifier of a volume is not part of it: ADR 0004 keeps one
/// store per configuration, and an item on another device is skipped rather than
/// copied.
///
/// # Errors
///
/// [`BrozaError::Config`] when the configured path is not absolute: every later
/// check (the mount table, the allowlist, the "inside the store" test) compares
/// absolute paths and a relative root would silently escape all three.
///
/// [`Config::quarantine_dir`]: crate::config::Config::quarantine_dir
pub fn store_root(configured: &Path) -> Result<PathBuf, BrozaError> {
    if !configured.is_absolute() {
        return Err(BrozaError::Config(format!(
            "quarantine-path `{}` must be an absolute path",
            configured.display()
        )));
    }
    Ok(configured.to_path_buf())
}

/// Directory holding one session: `<root>/<session id>`.
pub fn session_dir(root: &Path, session: &SessionId) -> PathBuf {
    root.join(session.as_str())
}

/// Manifest of a session: `<session dir>/manifest.json`.
pub fn manifest_path(session_dir: &Path) -> PathBuf {
    session_dir.join(MANIFEST_FILE)
}

/// Directory of one item: `<session dir>/items/<seq>`, the sequence zero-padded.
pub fn item_dir(session_dir: &Path, sequence: u32) -> PathBuf {
    session_dir.join(ITEMS_DIR).join(sequence_label(sequence))
}

/// Where one item is stored: `<session dir>/items/<seq>/<basename>`.
///
/// The basename is kept so a restore can put the item back under its own name and
/// so a human browsing the store recognises what is in it.
pub fn stored_path(session_dir: &Path, sequence: u32, basename: &OsStr) -> PathBuf {
    item_dir(session_dir, sequence).join(basename)
}

/// The sequence number as it appears on disk and in an [`EntryId`]: `0001`.
pub fn sequence_label(sequence: u32) -> String {
    format!("{sequence:0SEQUENCE_WIDTH$}")
}

/// Identifier of one entry: `<session id>/<seq>`.
///
/// # Errors
///
/// [`BrozaError::Usage`] when the pair does not form a valid [`EntryId`]; the
/// grammar is checked by the model, never assumed here.
pub fn entry_id(session: &SessionId, sequence: u32) -> Result<EntryId, BrozaError> {
    format!("{session}/{}", sequence_label(sequence)).parse()
}

/// The basename of `path`, or `item` when it has none (`/`, `..`).
///
/// A path without a file name cannot be quarantined in practice — the safety
/// kernel rejects `..` and never approves a volume root — but the store must
/// still produce a name rather than an empty path.
pub fn basename(path: &Path) -> &OsStr {
    path.file_name().unwrap_or_else(|| OsStr::new("item"))
}

/// Identifier for a session starting now: `cln_YYYYMMDDHHMMSS_xxxx`.
///
/// The suffix separates two sessions started in the same second. It is derived
/// from the sub-second part of the clock rather than from a random generator: the
/// core has no entropy source of its own, and a collision only matters inside one
/// second, where the nanosecond field already differs.
///
/// # Errors
///
/// [`BrozaError::Usage`] when the clock reports an instant that does not fit the
/// grammar of a [`SessionId`] (a year outside `0001..=9999`).
pub fn generate_session_id(clock: &dyn Clock) -> Result<SessionId, BrozaError> {
    let now = clock.now();
    let stamp = now.strftime(SESSION_STAMP_PATTERN).to_string();
    let suffix = now.subsec_nanosecond().unsigned_abs() % SUFFIX_SPACE;
    format!("{SESSION_PREFIX}_{stamp}_{suffix:04x}").parse()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use jiff::Timestamp;

    use super::{
        basename, entry_id, generate_session_id, item_dir, manifest_path, sequence_label, session_dir,
        store_root, stored_path,
    };
    use crate::BrozaError;
    use crate::model::SessionId;
    use crate::testing::FixedClock;

    const ROOT: &str = "/Users/dana/.local/share/broza/quarantine";

    fn session() -> SessionId {
        "cln_20260921103608_a1b2".parse().unwrap_or_else(|error| panic!("{error}"))
    }

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    #[test]
    fn a_session_lives_in_a_directory_named_after_its_identifier() {
        let dir = session_dir(Path::new(ROOT), &session());

        assert_eq!(dir, PathBuf::from(format!("{ROOT}/cln_20260921103608_a1b2")));
        assert_eq!(manifest_path(&dir), dir.join("manifest.json"));
    }

    #[test]
    fn an_item_directory_zero_pads_its_sequence_number() {
        let dir = session_dir(Path::new(ROOT), &session());

        assert_eq!(item_dir(&dir, 1), dir.join("items/0001"));
        assert_eq!(item_dir(&dir, 1284), dir.join("items/1284"));
        assert_eq!(sequence_label(7), "0007");
    }

    #[test]
    fn a_sequence_beyond_four_digits_keeps_all_of_them() {
        assert_eq!(sequence_label(123_456), "123456");
    }

    #[test]
    fn a_stored_item_keeps_the_basename_it_had() {
        let dir = session_dir(Path::new(ROOT), &session());

        let stored = stored_path(&dir, 2, OsStr::new("DerivedData"));

        assert_eq!(stored, dir.join("items/0002/DerivedData"));
    }

    #[test]
    fn a_path_without_a_file_name_still_gets_one() {
        assert_eq!(basename(Path::new("/Users/dana/a.cache")), OsStr::new("a.cache"));
        assert_eq!(basename(Path::new("/")), OsStr::new("item"));
    }

    #[test]
    fn an_entry_identifier_pairs_the_session_with_the_padded_sequence() {
        let id = entry_id(&session(), 1).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(id.as_str(), "cln_20260921103608_a1b2/0001");
        assert_eq!(id.session_part(), session().as_str());
    }

    #[test]
    fn a_relative_store_root_is_refused() {
        let error = store_root(Path::new("relative/quarantine"));

        assert!(matches!(error, Err(BrozaError::Config(_))), "{error:?}");
        assert_eq!(store_root(Path::new(ROOT)).ok(), Some(PathBuf::from(ROOT)));
    }

    #[test]
    fn a_generated_identifier_carries_the_clock_time() {
        let clock = FixedClock::at(at("2026-09-21T10:36:08Z"));

        let id = generate_session_id(&clock).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(id.timestamp_part(), "20260921103608");
        assert_eq!(id.suffix_part(), "0000", "a whole second has no sub-second part");
    }

    #[test]
    fn the_suffix_comes_from_the_sub_second_part_and_wraps() {
        let first = FixedClock::at(at("2026-09-21T10:36:08.000000001Z"));
        let second = FixedClock::at(at("2026-09-21T10:36:08.000065537Z"));

        let one = generate_session_id(&first).unwrap_or_else(|error| panic!("{error}"));
        let two = generate_session_id(&second).unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(one.timestamp_part(), two.timestamp_part());
        assert_eq!(one.suffix_part(), "0001");
        assert_eq!(two.suffix_part(), "0001", "the suffix wraps inside its 16-bit space");
        assert_ne!(one.suffix_part(), "0002");
    }

    #[test]
    fn a_generated_identifier_is_a_valid_session_identifier() {
        let clock = FixedClock::at(at("2026-12-31T23:59:59.999999999Z"));

        let id = generate_session_id(&clock).unwrap_or_else(|error| panic!("{error}"));

        assert!(id.as_str().parse::<SessionId>().is_ok(), "{id}");
    }
}
