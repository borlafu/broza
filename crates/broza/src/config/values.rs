//! Minimal validators for the string-typed configuration values.
//!
//! Configuration stores durations and sizes as validated `String`s so that the
//! TOML file round-trips byte-for-byte what the user wrote.
//!
// TODO(M0-A merge): switch the parsing here to `model::units` once it lands and
// keep only the "is this spelling acceptable" check in this file.

use crate::BrozaError;

/// Duration units accepted by `docs/cli-spec.md` §1.4 (`m` is months, never minutes).
const DURATION_UNITS: [char; 5] = ['h', 'd', 'w', 'm', 'y'];

/// Size units accepted by `docs/cli-spec.md` §1.4, longest first so that
/// `KiB` is matched before `B`.
const SIZE_UNITS: [&str; 9] = ["KIB", "MIB", "GIB", "TIB", "KB", "MB", "GB", "TB", "B"];

/// Validate a duration literal such as `24h`, `30d`, `6m`, `1y`.
///
/// Returns the trimmed literal unchanged so callers can store it verbatim.
pub fn validate_duration(raw: &str) -> Result<String, BrozaError> {
    let text = raw.trim();
    let Some(unit) = text.chars().last() else {
        return Err(duration_error(raw));
    };
    if !DURATION_UNITS.contains(&unit.to_ascii_lowercase()) {
        return Err(duration_error(raw));
    }
    let digits = &text[..text.len() - unit.len_utf8()];
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(duration_error(raw));
    }
    Ok(text.to_owned())
}

/// Validate a size literal such as `50MB`, `1.5GB`, `1GiB`.
pub fn validate_size(raw: &str) -> Result<String, BrozaError> {
    let text = raw.trim();
    let upper = text.to_ascii_uppercase();
    let Some(unit) = SIZE_UNITS.iter().find(|unit| upper.ends_with(*unit)) else {
        return Err(size_error(raw));
    };
    let number = upper[..upper.len() - unit.len()].trim();
    if number.is_empty() || number.parse::<f64>().is_err() || number.starts_with('-') {
        return Err(size_error(raw));
    }
    Ok(text.to_owned())
}

/// Validate a boolean literal (`true` / `false`).
pub fn validate_bool(raw: &str) -> Result<bool, BrozaError> {
    match raw.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(BrozaError::Usage(format!("expected `true` or `false`, got `{other}`"))),
    }
}

fn duration_error(raw: &str) -> BrozaError {
    BrozaError::Usage(format!(
        "invalid duration `{raw}`: expected <int><h|d|w|m|y>, for example `30d` or `1y`"
    ))
}

fn size_error(raw: &str) -> BrozaError {
    BrozaError::Usage(format!(
        "invalid size `{raw}`: expected <number><B|KB|MB|GB|TB|KiB|MiB|GiB|TiB>, for example `50MB`"
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn accepts_every_documented_duration_unit() {
        for literal in ["24h", "30d", "2w", "6m", "1y"] {
            assert_eq!(validate_duration(literal).unwrap(), literal);
        }
    }

    #[test]
    fn rejects_durations_without_unit_or_digits() {
        for literal in ["3", "", "d", "1x", "1.5d", "-1d"] {
            assert!(validate_duration(literal).is_err(), "{literal} should be rejected");
        }
    }

    #[test]
    fn accepts_decimal_and_binary_sizes_case_insensitively() {
        for literal in ["50MB", "1.5gb", "1GiB", "512B", "2 TB"] {
            assert_eq!(validate_size(literal).unwrap(), literal.trim());
        }
    }

    #[test]
    fn rejects_sizes_without_number_or_unit() {
        for literal in ["big", "", "100", "MB", "-5MB"] {
            assert!(validate_size(literal).is_err(), "{literal} should be rejected");
        }
    }

    #[test]
    fn parses_booleans_and_rejects_anything_else() {
        assert!(validate_bool("true").unwrap());
        assert!(!validate_bool("false").unwrap());
        assert!(validate_bool("yes").is_err());
    }
}
