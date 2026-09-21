//! RFC 3339 UTC timestamps for the JSON contract (`docs/cli-spec.md` §4.1).
//!
//! The contract carries whole seconds only, so serialisation truncates: a timestamp
//! read from a file system with nanosecond precision never becomes a moment in the
//! future. Deserialisation accepts any RFC 3339 instant.

use jiff::{RoundMode, Timestamp, TimestampRound, Unit};
use serde::{Deserialize, Deserializer, Serializer};

/// Truncate `timestamp` to whole seconds, leaving it untouched if that fails.
fn whole_seconds(timestamp: Timestamp) -> Timestamp {
    let options = TimestampRound::new().smallest(Unit::Second).mode(RoundMode::Trunc);
    timestamp.round(options).unwrap_or(timestamp)
}

/// Serialise a timestamp as an RFC 3339 UTC string with whole seconds.
pub(crate) fn serialize<S: Serializer>(timestamp: &Timestamp, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&whole_seconds(*timestamp).to_string())
}

/// Deserialise an RFC 3339 timestamp.
pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Timestamp, D::Error> {
    Timestamp::deserialize(deserializer)
}

/// The same representation for an optional timestamp.
pub(crate) mod optional {
    use super::{Deserialize, Deserializer, Serializer, Timestamp, whole_seconds};

    /// Serialise `Some(timestamp)` as an RFC 3339 UTC string, `None` as `null`.
    // `serde(with = ...)` dictates the `&Option<T>` signature.
    #[allow(clippy::ref_option)]
    pub(crate) fn serialize<S: Serializer>(
        timestamp: &Option<Timestamp>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match timestamp {
            Some(value) => serializer.serialize_str(&whole_seconds(*value).to_string()),
            None => serializer.serialize_none(),
        }
    }

    /// Deserialise an optional RFC 3339 timestamp.
    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Timestamp>, D::Error> {
        Option::<Timestamp>::deserialize(deserializer)
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Holder {
        #[serde(with = "super")]
        at: Timestamp,
        #[serde(default, with = "super::optional", skip_serializing_if = "Option::is_none")]
        maybe: Option<Timestamp>,
    }

    fn parse(raw: &str) -> Timestamp {
        raw.parse().unwrap_or_else(|e| panic!("{raw}: {e}"))
    }

    #[test]
    fn whole_seconds_round_trip_unchanged() {
        let holder = Holder { at: parse("2026-09-21T10:36:08Z"), maybe: None };
        let json = serde_json::to_value(&holder).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(json, serde_json::json!({"at": "2026-09-21T10:36:08Z"}));
        let back: Holder = serde_json::from_value(json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back, holder);
    }

    #[test]
    fn sub_second_precision_is_truncated_never_rounded_up() {
        let holder =
            Holder { at: parse("2026-09-21T10:36:08.999999Z"), maybe: Some(parse("2026-01-01T00:00:00.5Z")) };
        let json = serde_json::to_value(&holder).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(json["at"], "2026-09-21T10:36:08Z");
        assert_eq!(json["maybe"], "2026-01-01T00:00:00Z");
    }

    #[test]
    fn an_absent_optional_timestamp_serializes_as_null_when_it_is_not_skipped() {
        #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
        struct Nullable {
            #[serde(with = "super::optional")]
            maybe: Option<Timestamp>,
        }
        let json = serde_json::to_value(Nullable { maybe: None }).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(json, serde_json::json!({"maybe": serde_json::Value::Null}));
        let back: Nullable = serde_json::from_value(json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back.maybe, None);
    }

    #[test]
    fn a_non_rfc_3339_value_is_rejected() {
        assert!(serde_json::from_str::<Holder>("{\"at\":\"21/09/2026\"}").is_err());
        assert!(serde_json::from_str::<Holder>("{\"at\":1758450968}").is_err());
    }
}
