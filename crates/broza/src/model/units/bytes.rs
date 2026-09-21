//! Byte sizes and their `<number><unit>` grammar (`docs/cli-spec.md` §1.4).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::BrozaError;

/// Decimal (SI) size units, smallest first. `1 GB` is 10^9 bytes.
const DECIMAL_UNITS: [(&str, u128); 5] =
    [("b", 1), ("kb", 1_000), ("mb", 1_000_000), ("gb", 1_000_000_000), ("tb", 1_000_000_000_000)];

/// Binary (IEC) size units, smallest first. `1 GiB` is 2^30 bytes.
const BINARY_UNITS: [(&str, u128); 4] =
    [("kib", 1_024), ("mib", 1_048_576), ("gib", 1_073_741_824), ("tib", 1_099_511_627_776)];

/// Canonical decimal unit suffixes used by [`ByteSize`]'s [`fmt::Display`], smallest first.
const CANONICAL_UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

/// A size in bytes.
///
/// All sizes in Broza are unsigned byte counts; formatting for humans belongs to the
/// presentation layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteSize(u64);

impl ByteSize {
    /// Build a size from a raw byte count.
    pub const fn new(bytes: u64) -> Self {
        Self(bytes)
    }

    /// The raw byte count.
    pub const fn bytes(self) -> u64 {
        self.0
    }
}

impl From<u64> for ByteSize {
    fn from(bytes: u64) -> Self {
        Self(bytes)
    }
}

impl From<ByteSize> for u64 {
    fn from(size: ByteSize) -> Self {
        size.0
    }
}

impl fmt::Display for ByteSize {
    /// Canonical form: the largest decimal unit that divides the value exactly.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let exact = DECIMAL_UNITS
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (_, factor))| self.0 != 0 && u128::from(self.0) % factor == 0);
        match exact {
            Some((index, (_, factor))) => {
                let scaled = u128::from(self.0) / factor;
                write!(f, "{scaled}{}", CANONICAL_UNITS.get(index).copied().unwrap_or("B"))
            }
            None => write!(f, "{}B", self.0),
        }
    }
}

impl FromStr for ByteSize {
    type Err = BrozaError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let trimmed = raw.trim();
        let invalid = |reason: &str| BrozaError::Usage(format!("invalid size `{raw}`: {reason}"));
        let boundary = trimmed
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .ok_or_else(|| invalid("expected a unit such as MB, GB or GiB"))?;
        let (number, unit) = trimmed.split_at(boundary);
        let factor = unit_factor(unit.trim_start())
            .ok_or_else(|| invalid("unknown unit, expected B, KB, MB, GB, TB, KiB, MiB, GiB or TiB"))?;
        let bytes = scale_decimal(number, factor).ok_or_else(|| invalid("not a byte count in range"))?;
        Ok(Self(bytes))
    }
}

/// Look up the byte factor of a unit suffix, case-insensitively.
fn unit_factor(unit: &str) -> Option<u128> {
    let lowered = unit.to_ascii_lowercase();
    DECIMAL_UNITS
        .iter()
        .chain(BINARY_UNITS.iter())
        .find(|(name, _)| *name == lowered)
        .map(|(_, factor)| *factor)
}

/// Multiply the decimal literal `number` by `factor`, truncating towards zero.
///
/// Returns `None` for a malformed literal or a result outside `u64`.
fn scale_decimal(number: &str, factor: u128) -> Option<u64> {
    let (integer, fraction) = number.split_once('.').unwrap_or((number, ""));
    if integer.is_empty() || (number.contains('.') && fraction.is_empty()) {
        return None;
    }
    if !integer.bytes().chain(fraction.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let digits: u128 = format!("{integer}{fraction}").parse().ok()?;
    let divisor = 10_u128.checked_pow(u32::try_from(fraction.len()).ok()?)?;
    u64::try_from(digits.checked_mul(factor)? / divisor).ok()
}

impl Serialize for ByteSize {
    /// Serialises as the canonical string form (`50MB`).
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    /// Accepts either the string form (`"50MB"`) or a raw non-negative byte count.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ByteSizeVisitor)
    }
}

/// Visitor backing [`ByteSize`]'s [`Deserialize`] implementation.
struct ByteSizeVisitor;

impl de::Visitor<'_> for ByteSizeVisitor {
    type Value = ByteSize;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a size string such as \"50MB\" or a byte count")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        value.parse().map_err(de::Error::custom)
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(ByteSize(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        u64::try_from(value).map(ByteSize).map_err(|_| de::Error::custom("size must not be negative"))
    }
}

#[cfg(test)]
mod tests {
    use super::ByteSize;
    use std::str::FromStr;

    fn size(raw: &str) -> u64 {
        ByteSize::from_str(raw).unwrap_or_else(|e| panic!("{raw}: {e}")).bytes()
    }

    #[test]
    fn byte_size_parses_decimal_and_binary_units() {
        let cases = [
            ("0B", 0_u64),
            ("512B", 512),
            ("1KB", 1_000),
            ("1MB", 1_000_000),
            ("1GB", 1_000_000_000),
            ("1TB", 1_000_000_000_000),
            ("1KiB", 1_024),
            ("1MiB", 1_048_576),
            ("1GiB", 1_073_741_824),
            ("1TiB", 1_099_511_627_776),
        ];
        for (raw, expected) in cases {
            assert_eq!(size(raw), expected, "{raw}");
        }
    }

    #[test]
    fn byte_size_accepts_decimals_spaces_and_any_case() {
        let cases = [
            ("1.5GB", 1_500_000_000_u64),
            ("1.5 GB", 1_500_000_000),
            ("  1.5gb  ", 1_500_000_000),
            ("1.5GiB", 1_610_612_736),
            ("0.5kb", 500),
            ("50MB", 50_000_000),
            ("2.25 mib", 2_359_296),
            ("1.999B", 1),
            ("1.0TB", 1_000_000_000_000),
        ];
        for (raw, expected) in cases {
            assert_eq!(size(raw), expected, "{raw}");
        }
    }

    #[test]
    fn byte_size_rejects_invalid_input() {
        let cases = [
            ("", "empty"),
            ("   ", "blank"),
            ("100", "no unit"),
            ("-1GB", "negative"),
            ("+1GB", "signed"),
            ("GB", "no number"),
            ("1XB", "unknown unit"),
            ("1.2.3GB", "two dots"),
            ("1,5GB", "comma decimal"),
            ("1GB extra", "trailing text"),
            ("99999999999999999999GB", "overflow"),
            ("18446744073709551616B", "u64 overflow"),
        ];
        for (raw, why) in cases {
            assert!(ByteSize::from_str(raw).is_err(), "expected error for {why}: {raw:?}");
        }
    }

    #[test]
    fn byte_size_displays_a_round_tripping_canonical_form() {
        let cases = [
            (0_u64, "0B"),
            (512, "512B"),
            (1_500, "1500B"),
            (1_000, "1KB"),
            (50_000_000, "50MB"),
            (1_000_000_000_000, "1TB"),
            (1_073_741_824, "1073741824B"),
        ];
        for (bytes, expected) in cases {
            let value = ByteSize::new(bytes);
            assert_eq!(value.to_string(), expected, "{bytes}");
            assert_eq!(size(expected), bytes, "round trip {expected}");
        }
    }

    #[test]
    fn byte_size_serializes_as_a_string_and_accepts_integers() {
        let value = ByteSize::new(50_000_000);
        let json = serde_json::to_string(&value).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(json, "\"50MB\"");
        let from_string: ByteSize = serde_json::from_str("\"50MB\"").unwrap_or_else(|e| panic!("{e}"));
        let from_integer: ByteSize = serde_json::from_str("50000000").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(from_string, value);
        assert_eq!(from_integer, value);
        assert_eq!(u64::from(value), 50_000_000);
        assert_eq!(ByteSize::from(7_u64).bytes(), 7);
        assert!(serde_json::from_str::<ByteSize>("-5").is_err());
        assert!(serde_json::from_str::<ByteSize>("\"nope\"").is_err());
    }
}
