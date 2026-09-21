//! Stable string identifiers of the JSON contract (`docs/cli-spec.md` §4).
//!
//! Every identifier is a validated newtype over [`String`] that serialises as a plain
//! JSON string. Parsing happens at the system boundary: an identifier value can only
//! exist if it matches the grammar in the specification.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::BrozaError;

/// Prefix of every cleanup session identifier.
const SESSION_ID_PREFIX: &str = "cln_";
/// Length of the `YYYYMMDDHHMMSS` part of a session identifier.
const SESSION_TIMESTAMP_LEN: usize = 14;
/// Length of the random suffix of a session identifier.
const SESSION_SUFFIX_LEN: usize = 4;
/// Separator between the category and the detector inside a finding identifier.
const FINDING_ID_SEPARATOR: char = '.';
/// Separator between the session and the sequence number inside an entry identifier.
const ENTRY_ID_SEPARATOR: char = '/';
/// `strptime` pattern of the timestamp part of a session identifier.
const SESSION_TIMESTAMP_PATTERN: &str = "%Y%m%d%H%M%S";

/// Implement the shared surface of a validated string identifier.
///
/// The type must also implement [`FromStr`] with `Err = BrozaError`; both
/// [`Deserialize`] and [`TryFrom`] route through it so that no unvalidated value
/// can be built.
macro_rules! string_id {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Borrow the identifier as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume the identifier and return the inner [`String`].
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = BrozaError;

            fn try_from(raw: &str) -> Result<Self, Self::Error> {
                raw.parse()
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                raw.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

string_id!(
    SessionId,
    "Identifier of a cleanup session: `cln_YYYYMMDDHHMMSS_xxxx` (`docs/cli-spec.md` §3.5)."
);
string_id!(
    FindingId,
    "Identifier of a finding: `<category>.<detector>`, both parts kebab-case (`docs/cli-spec.md` §4.3)."
);
string_id!(
    VolumeId,
    "BSD device identifier such as `disk0`, `disk3` or `disk3s1`.\n\nUsed for disks, containers and volumes: macOS names all three in the same namespace."
);
string_id!(EntryId, "Identifier of one quarantined item: `<session id>/<seq>` (`docs/cli-spec.md` §3.5).");

impl SessionId {
    /// The `YYYYMMDDHHMMSS` part of the identifier.
    pub fn timestamp_part(&self) -> &str {
        let start = SESSION_ID_PREFIX.len();
        self.0.get(start..start + SESSION_TIMESTAMP_LEN).unwrap_or_default()
    }

    /// The random suffix of the identifier.
    pub fn suffix_part(&self) -> &str {
        let start = SESSION_ID_PREFIX.len() + SESSION_TIMESTAMP_LEN + 1;
        self.0.get(start..).unwrap_or_default()
    }
}

impl FromStr for SessionId {
    type Err = BrozaError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let invalid =
            || BrozaError::Usage(format!("invalid session id `{raw}`, expected cln_YYYYMMDDHHMMSS_xxxx"));
        let rest = raw.strip_prefix(SESSION_ID_PREFIX).ok_or_else(invalid)?;
        let (stamp, suffix) = rest.split_once('_').ok_or_else(invalid)?;
        if !is_valid_session_timestamp(stamp) || !is_valid_session_suffix(suffix) {
            return Err(invalid());
        }
        Ok(Self(raw.to_owned()))
    }
}

impl FromStr for FindingId {
    type Err = BrozaError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let invalid =
            || BrozaError::Usage(format!("invalid finding id `{raw}`, expected <category>.<detector>"));
        let (category, detector) = raw.split_once(FINDING_ID_SEPARATOR).ok_or_else(invalid)?;
        if !is_kebab_case(category) || !is_kebab_case(detector) {
            return Err(invalid());
        }
        Ok(Self(raw.to_owned()))
    }
}

impl FindingId {
    /// The category part of the identifier (everything before the first `.`).
    pub fn category_part(&self) -> &str {
        self.0.split_once(FINDING_ID_SEPARATOR).map_or("", |(category, _)| category)
    }

    /// The detector part of the identifier (everything after the first `.`).
    pub fn detector_part(&self) -> &str {
        self.0.split_once(FINDING_ID_SEPARATOR).map_or("", |(_, detector)| detector)
    }
}

impl FromStr for EntryId {
    type Err = BrozaError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let invalid = || BrozaError::Usage(format!("invalid entry id `{raw}`, expected <session id>/<seq>"));
        let (session, sequence) = raw.split_once(ENTRY_ID_SEPARATOR).ok_or_else(invalid)?;
        SessionId::from_str(session).map_err(|_| invalid())?;
        if sequence.is_empty() || !sequence.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        Ok(Self(raw.to_owned()))
    }
}

impl EntryId {
    /// The session part of the identifier.
    pub fn session_part(&self) -> &str {
        self.0.split_once(ENTRY_ID_SEPARATOR).map_or("", |(session, _)| session)
    }

    /// The sequence part of the identifier.
    pub fn sequence_part(&self) -> &str {
        self.0.split_once(ENTRY_ID_SEPARATOR).map_or("", |(_, sequence)| sequence)
    }
}

impl FromStr for VolumeId {
    type Err = BrozaError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if !is_bsd_device_name(raw) {
            return Err(BrozaError::Usage(format!("invalid volume id `{raw}`, expected a BSD device name")));
        }
        Ok(Self(raw.to_owned()))
    }
}

/// `true` when `value` is a lowercase kebab-case segment (`user-cache`).
fn is_kebab_case(value: &str) -> bool {
    let bytes = value.as_bytes();
    let is_alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    match (bytes.first(), bytes.last()) {
        (Some(&first), Some(&last)) if is_alnum(first) && is_alnum(last) => {}
        _ => return false,
    }
    bytes.iter().all(|&b| is_alnum(b) || b == b'-')
}

/// `true` when `value` is a real calendar date and time written as `YYYYMMDDHHMMSS`.
///
/// Formatting the parsed value back must reproduce `value` exactly: `jiff` accepts a
/// leap second and normalises it, and a session identifier must be verbatim.
fn is_valid_session_timestamp(value: &str) -> bool {
    if value.len() != SESSION_TIMESTAMP_LEN || !value.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    jiff::civil::DateTime::strptime(SESSION_TIMESTAMP_PATTERN, value)
        .is_ok_and(|parsed| parsed.strftime(SESSION_TIMESTAMP_PATTERN).to_string() == value)
}

/// `true` when `value` is the four-character lowercase random suffix.
fn is_valid_session_suffix(value: &str) -> bool {
    value.len() == SESSION_SUFFIX_LEN && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// `true` when `value` looks like `disk<N>` optionally followed by `s<N>` slices.
fn is_bsd_device_name(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("disk") else {
        return false;
    };
    let mut parts = rest.split('s');
    let all_numeric = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    parts.next().is_some_and(all_numeric) && parts.all(all_numeric)
}

#[cfg(test)]
mod tests {
    use crate::model::ids::{EntryId, FindingId, SessionId, VolumeId};
    use std::str::FromStr;

    fn session(raw: &str) -> SessionId {
        SessionId::from_str(raw).unwrap_or_else(|e| panic!("{raw}: {e}"))
    }

    #[test]
    fn session_id_accepts_the_specified_grammar() {
        let id = session("cln_20260921103608_a1b2");
        assert_eq!(id.as_str(), "cln_20260921103608_a1b2");
        assert_eq!(id.to_string(), "cln_20260921103608_a1b2");
        assert_eq!(id.timestamp_part(), "20260921103608");
        assert_eq!(id.suffix_part(), "a1b2");
    }

    #[test]
    fn session_id_rejects_malformed_values() {
        let cases = [
            ("", "empty"),
            ("cln_20260921103608", "no suffix"),
            ("20260921103608_a1b2", "no prefix"),
            ("cln_2026092110360_a1b2", "short timestamp"),
            ("cln_2026092110360x_a1b2", "non digit timestamp"),
            ("cln_20261321103608_a1b2", "month 13"),
            ("cln_20260900103608_a1b2", "day 0"),
            ("cln_20260921243608_a1b2", "hour 24"),
            ("cln_20260921106008_a1b2", "minute 60"),
            ("cln_20260921103660_a1b2", "second 60"),
            ("cln_20260921103608_A1B2", "uppercase suffix"),
            ("cln_20260921103608_a1b", "short suffix"),
            ("cln_20260921103608_a1b2z", "long suffix"),
            ("cln_20260230103608_a1b2", "30 February"),
            ("cln_20250229103608_a1b2", "29 February in a common year"),
            ("cln_20260931103608_a1b2", "31 September"),
        ];
        for (raw, why) in cases {
            assert!(SessionId::from_str(raw).is_err(), "expected error for {why}: {raw}");
        }
    }

    #[test]
    fn finding_id_splits_category_and_detector() {
        let id = FindingId::from_str("build-cache.xcode-deriveddata").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(id.category_part(), "build-cache");
        assert_eq!(id.detector_part(), "xcode-deriveddata");
        assert_eq!(id.to_string(), "build-cache.xcode-deriveddata");
    }

    #[test]
    fn finding_id_rejects_non_kebab_case_values() {
        let cases = [
            "",
            "build-cache",
            "build-cache.xcode.deriveddata",
            ".detector",
            "category.",
            "Build-Cache.detector",
            "build_cache.detector",
            "-build.detector",
            "build-.detector",
            "build cache.detector",
        ];
        for raw in cases {
            assert!(FindingId::from_str(raw).is_err(), "expected error for {raw:?}");
        }
    }

    #[test]
    fn volume_id_accepts_bsd_device_names() {
        for raw in ["disk0", "disk3", "disk3s1", "disk3s1s2"] {
            let id = VolumeId::from_str(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!(id.as_str(), raw);
        }
    }

    #[test]
    fn volume_id_rejects_other_names() {
        for raw in ["", "disk", "diskx", "sda1", "/dev/disk3s1", "disk3s", "disks1", "DISK0"] {
            assert!(VolumeId::from_str(raw).is_err(), "expected error for {raw:?}");
        }
    }

    #[test]
    fn identifiers_serialize_as_plain_strings() {
        let id = session("cln_20260921103608_a1b2");
        let json = serde_json::to_string(&id).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(json, "\"cln_20260921103608_a1b2\"");
        let back: SessionId = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back, id);
    }

    #[test]
    fn deserializing_an_invalid_identifier_fails_without_panicking() {
        assert!(serde_json::from_str::<FindingId>("\"NOT VALID\"").is_err());
        assert!(serde_json::from_str::<VolumeId>("42").is_err());
    }

    #[test]
    fn identifiers_expose_their_inner_string() {
        let id = FindingId::from_str("user-cache.logs").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(id.clone().into_inner(), "user-cache.logs");
        assert_eq!(id.as_ref(), "user-cache.logs");
    }

    #[test]
    fn entry_ids_pair_a_session_with_a_sequence_number() {
        let id = EntryId::from_str("cln_20260917103608_a1b2/0001").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(id.session_part(), "cln_20260917103608_a1b2");
        assert_eq!(id.sequence_part(), "0001");
        assert_eq!(id.to_string(), "cln_20260917103608_a1b2/0001");
    }

    #[test]
    fn entry_ids_reject_a_missing_or_malformed_part() {
        let cases = [
            "",
            "cln_20260917103608_a1b2",
            "cln_20260917103608_a1b2/",
            "cln_20260917103608_a1b2/x1",
            "/0001",
            "not-a-session/0001",
            "cln_20261317103608_a1b2/0001",
        ];
        for raw in cases {
            assert!(EntryId::from_str(raw).is_err(), "expected error for {raw:?}");
        }
    }

    #[test]
    fn identifiers_can_be_converted_from_string_slices() {
        let id = VolumeId::try_from("disk3s5").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(id.as_str(), "disk3s5");
        assert!(VolumeId::try_from("nvme0").is_err());
    }
}
