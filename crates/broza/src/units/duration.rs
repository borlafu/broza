//! Durations and their `<int><unit>` grammar (`docs/cli-spec.md` §1.4).

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::BrozaError;

/// Seconds in an hour.
const SECONDS_PER_HOUR: u64 = 3_600;
/// Seconds in a day.
const SECONDS_PER_DAY: u64 = 86_400;
/// Days in a week.
const DAYS_PER_WEEK: u64 = 7;
/// Days in a month, as defined by the specification.
const DAYS_PER_MONTH: u64 = 30;
/// Days in a year, as defined by the specification.
const DAYS_PER_YEAR: u64 = 365;

/// Unit of a [`DurationSpec`]. Minutes are not supported: `m` always means months.
///
/// A closed set: this is the command-line grammar of `docs/cli-spec.md` §1.4, not a
/// JSON contract enum, so adding a unit is a deliberate, breaking grammar change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurationUnit {
    /// Hours (`h`).
    Hours,
    /// Days (`d`).
    Days,
    /// Weeks of 7 days (`w`).
    Weeks,
    /// Months of 30 days (`m`).
    Months,
    /// Years of 365 days (`y`).
    Years,
}

impl DurationUnit {
    /// Number of seconds in one unit.
    pub const fn seconds(self) -> u64 {
        match self {
            Self::Hours => SECONDS_PER_HOUR,
            Self::Days => SECONDS_PER_DAY,
            Self::Weeks => SECONDS_PER_DAY * DAYS_PER_WEEK,
            Self::Months => SECONDS_PER_DAY * DAYS_PER_MONTH,
            Self::Years => SECONDS_PER_DAY * DAYS_PER_YEAR,
        }
    }

    /// The single-character suffix used in the grammar.
    pub const fn suffix(self) -> char {
        match self {
            Self::Hours => 'h',
            Self::Days => 'd',
            Self::Weeks => 'w',
            Self::Months => 'm',
            Self::Years => 'y',
        }
    }

    /// Parse a suffix character. Suffixes are lowercase only.
    pub const fn from_suffix(suffix: char) -> Option<Self> {
        match suffix {
            'h' => Some(Self::Hours),
            'd' => Some(Self::Days),
            'w' => Some(Self::Weeks),
            'm' => Some(Self::Months),
            'y' => Some(Self::Years),
            _ => None,
        }
    }
}

/// A duration written as `<int><unit>`, for example `30d`, `6m` or `1y`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DurationSpec {
    /// Number of units.
    amount: u64,
    /// Unit the amount is expressed in.
    unit: DurationUnit,
}

impl DurationSpec {
    /// Build a specification, rejecting amounts whose total exceeds `u64` seconds.
    pub fn new(amount: u64, unit: DurationUnit) -> Result<Self, BrozaError> {
        if amount.checked_mul(unit.seconds()).is_none() {
            return Err(BrozaError::Usage(format!("duration `{amount}{}` is out of range", unit.suffix())));
        }
        Ok(Self { amount, unit })
    }

    /// Number of units.
    pub const fn amount(self) -> u64 {
        self.amount
    }

    /// Unit the amount is expressed in.
    pub const fn unit(self) -> DurationUnit {
        self.unit
    }

    /// Total number of seconds. Always exact: the constructors reject overflow.
    pub const fn total_seconds(self) -> u64 {
        self.amount.saturating_mul(self.unit.seconds())
    }

    /// The duration as a [`std::time::Duration`].
    pub const fn to_duration(self) -> Duration {
        Duration::from_secs(self.total_seconds())
    }

    /// Whole days contained in the duration, truncated (`36h` is one day).
    pub const fn whole_days(self) -> u64 {
        self.total_seconds() / SECONDS_PER_DAY
    }
}

impl Default for DurationSpec {
    /// The neutral duration, `0h`.
    ///
    /// Exists so callers that know their amount cannot overflow — configuration
    /// defaults, for instance — can write `DurationSpec::new(..).unwrap_or_default()`
    /// instead of carrying an unreachable error path.
    fn default() -> Self {
        Self { amount: 0, unit: DurationUnit::Hours }
    }
}

impl fmt::Display for DurationSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.amount, self.unit.suffix())
    }
}

impl From<DurationSpec> for Duration {
    fn from(spec: DurationSpec) -> Self {
        spec.to_duration()
    }
}

impl FromStr for DurationSpec {
    type Err = BrozaError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let trimmed = raw.trim();
        let invalid = |reason: &str| BrozaError::Usage(format!("invalid duration `{raw}`: {reason}"));
        let suffix = trimmed.chars().next_back().ok_or_else(|| invalid("value is empty"))?;
        let unit =
            DurationUnit::from_suffix(suffix).ok_or_else(|| invalid("expected a unit: h, d, w, m or y"))?;
        let amount = trimmed
            .get(..trimmed.len() - suffix.len_utf8())
            .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|digits| digits.parse::<u64>().ok())
            .ok_or_else(|| invalid("expected a whole number before the unit"))?;
        Self::new(amount, unit)
    }
}

impl Serialize for DurationSpec {
    /// Serialises as the canonical string form (`30d`).
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for DurationSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{DurationSpec, DurationUnit};
    use std::str::FromStr;
    use std::time::Duration;

    #[test]
    fn the_default_duration_is_zero_hours() {
        let default = DurationSpec::default();
        assert_eq!(default.total_seconds(), 0);
        assert_eq!(default.to_string(), "0h");
        assert_eq!(DurationSpec::from_str("0h").unwrap_or_default(), default);
    }

    #[test]
    fn duration_spec_parses_every_unit() {
        let cases = [
            ("24h", DurationUnit::Hours, 24_u64, 86_400_u64, 1_u64),
            ("30d", DurationUnit::Days, 30, 2_592_000, 30),
            ("2w", DurationUnit::Weeks, 2, 1_209_600, 14),
            ("6m", DurationUnit::Months, 6, 15_552_000, 180),
            ("1y", DurationUnit::Years, 1, 31_536_000, 365),
            ("0d", DurationUnit::Days, 0, 0, 0),
        ];
        for (raw, unit, amount, secs, days) in cases {
            let spec = DurationSpec::from_str(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!(spec.unit(), unit, "{raw}");
            assert_eq!(spec.amount(), amount, "{raw}");
            assert_eq!(spec.to_duration(), Duration::from_secs(secs), "{raw}");
            assert_eq!(Duration::from(spec), Duration::from_secs(secs), "{raw}");
            assert_eq!(spec.whole_days(), days, "{raw}");
            assert_eq!(spec.to_string(), raw, "{raw}");
        }
    }

    #[test]
    fn duration_spec_truncates_partial_days() {
        let spec = DurationSpec::from_str("36h").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(spec.whole_days(), 1);
    }

    #[test]
    fn duration_spec_rejects_invalid_input() {
        let cases = [
            ("", "empty"),
            ("   ", "blank"),
            ("30", "no unit"),
            ("d", "no amount"),
            ("-1d", "negative"),
            ("1.5d", "fractional"),
            ("30s", "unknown unit"),
            ("30min", "minutes"),
            ("30D", "uppercase unit"),
            ("30 d", "inner space"),
            ("99999999999999999999y", "overflow"),
            ("1000000000000y", "seconds overflow"),
        ];
        for (raw, why) in cases {
            assert!(DurationSpec::from_str(raw).is_err(), "expected error for {why}: {raw:?}");
        }
        assert!(DurationSpec::new(u64::MAX, DurationUnit::Years).is_err());
    }

    #[test]
    fn duration_spec_serializes_as_its_canonical_string() {
        let spec = DurationSpec::from_str("30d").unwrap_or_else(|e| panic!("{e}"));
        let json = serde_json::to_string(&spec).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(json, "\"30d\"");
        let back: DurationSpec = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back, spec);
        assert!(serde_json::from_str::<DurationSpec>("\"30\"").is_err());
    }
}
