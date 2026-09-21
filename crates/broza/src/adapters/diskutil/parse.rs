//! Shared plumbing of the `diskutil` plist parsers.
//!
//! Every parser here is pure: bytes in, data out, no process and no filesystem.
//! The rule for all of them is the one in
//! `docs/adr/0002-diskutil-plist-over-diskarbitration.md`: `#[serde(default)]`
//! everywhere and unknown keys ignored, so a new macOS release that adds a key,
//! or drops one Broza does not depend on, keeps working.

use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};

use crate::BrozaError;

/// Deserialize a property list, naming the command whose output failed to parse.
pub(crate) fn parse_plist<T: DeserializeOwned>(bytes: &[u8], command: &str) -> Result<T, BrozaError> {
    plist::from_bytes(bytes)
        .map_err(|error| BrozaError::Other(format!("cannot parse the output of `{command}`: {error}")))
}

/// Read a path that `diskutil` writes as an empty string when it has none.
///
/// `MountPoint` is always present in the output; an unmounted volume gets
/// `<string></string>` rather than no key at all, and an empty path is not a
/// path (`docs/cli-spec.md` §4.2: the field is absent, never empty).
pub(crate) fn optional_path<'de, D>(deserializer: D) -> Result<Option<PathBuf>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(optional_text(deserializer)?.map(PathBuf::from))
}

/// Read a string that `diskutil` writes as empty when it has no value.
pub(crate) fn optional_text<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.filter(|value| !value.is_empty()))
}

/// Read a byte count without letting a surprising shape end the scan.
///
/// Every size in the contract is a `u64` (`AGENTS.md` §6), but a property list
/// can hold a negative integer, a real, or a string where a count is expected —
/// `diskutil` has shipped all three over the years for devices it cannot
/// measure. None of them is a size, and refusing to enumerate the machine
/// because one field is odd would be worse than reporting that field as zero.
/// The value is clamped, never guessed: nothing here invents a number.
pub(crate) fn lenient_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(as_u64(&plist::Value::deserialize(deserializer)?))
}

/// The byte count a property-list value represents, or zero.
fn as_u64(value: &plist::Value) -> u64 {
    match value {
        plist::Value::Integer(number) => {
            number.as_unsigned().or_else(|| number.as_signed().map(|_| 0)).unwrap_or(0)
        }
        plist::Value::Real(number) => {
            if number.is_finite() && *number >= 0.0 {
                // `as` saturates at u64::MAX for anything larger, which is the
                // clamp this function promises.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                {
                    *number as u64
                }
            } else {
                0
            }
        }
        plist::Value::String(text) => text.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde::Deserialize;

    use super::{lenient_u64, optional_path, optional_text, parse_plist};
    use crate::BrozaError;

    /// A plist with one key of each shape the helpers have to survive.
    const SAMPLE: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>MountPoint</key>
    <string></string>
    <key>VolumeName</key>
    <string>Data</string>
    <key>SomethingBrozaDoesNotKnow</key>
    <string>ignored</string>
</dict>
</plist>"#;

    #[derive(Debug, Default, Deserialize)]
    #[serde(rename_all = "PascalCase", default)]
    struct Sample {
        #[serde(deserialize_with = "optional_path")]
        mount_point: Option<PathBuf>,
        #[serde(deserialize_with = "optional_text")]
        volume_name: Option<String>,
        #[serde(deserialize_with = "optional_text")]
        media_name: Option<String>,
    }

    #[test]
    fn an_empty_value_is_no_value_and_an_unknown_key_is_ignored() {
        let parsed: Sample = parse_plist(SAMPLE, "diskutil info").unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(parsed.mount_point, None);
        assert_eq!(parsed.volume_name.as_deref(), Some("Data"));
        assert_eq!(parsed.media_name, None, "a missing key is not an error");
    }

    /// Every shape a size has been seen in, valid or not.
    const SIZES: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>Plain</key><integer>1024</integer>
    <key>Negative</key><integer>-1</integer>
    <key>Text</key><string> 2048 </string>
    <key>Nonsense</key><string>a lot</string>
    <key>Real</key><real>4096.7</real>
    <key>NegativeReal</key><real>-4096.0</real>
    <key>Flag</key><true/>
</dict>
</plist>"#;

    #[derive(Debug, Default, Deserialize)]
    #[serde(rename_all = "PascalCase", default)]
    struct Sizes {
        #[serde(deserialize_with = "lenient_u64")]
        plain: u64,
        #[serde(deserialize_with = "lenient_u64")]
        negative: u64,
        #[serde(deserialize_with = "lenient_u64")]
        text: u64,
        #[serde(deserialize_with = "lenient_u64")]
        nonsense: u64,
        #[serde(deserialize_with = "lenient_u64")]
        real: u64,
        #[serde(deserialize_with = "lenient_u64")]
        negative_real: u64,
        #[serde(deserialize_with = "lenient_u64")]
        flag: u64,
        #[serde(deserialize_with = "lenient_u64")]
        absent: u64,
    }

    #[test]
    fn a_size_that_is_not_a_positive_number_is_read_as_zero() {
        let parsed: Sizes = parse_plist(SIZES, "diskutil info").unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(parsed.plain, 1024);
        assert_eq!(parsed.text, 2048, "a number written as a string is still a number");
        assert_eq!(parsed.real, 4096);
        assert_eq!(
            (parsed.negative, parsed.nonsense, parsed.negative_real, parsed.flag, parsed.absent),
            (0, 0, 0, 0, 0)
        );
    }

    #[test]
    fn a_malformed_plist_names_the_command_that_produced_it() {
        let err = parse_plist::<Sample>(b"not a plist at all", "diskutil list -plist").err();

        let Some(BrozaError::Other(message)) = err else { panic!("expected BrozaError::Other") };
        assert!(message.contains("diskutil list -plist"), "{message}");
    }
}
