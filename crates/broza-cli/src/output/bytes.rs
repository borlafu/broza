//! Human-readable byte formatting (`docs/cli-spec.md` §1.4).
//!
//! Output is always **decimal** (SI), matching Finder and Disk Utility:
//! `1 GB` is 1,000,000,000 bytes. JSON never uses these strings; it carries
//! integer bytes.
//!
//! Precision, per unit:
//!
//! | Unit | Decimals | Example |
//! |---|---|---|
//! | `B` | 0 | `512 B` |
//! | `KB`, `MB`, `GB` | 1 | `138.2 GB` |
//! | `TB` | 2 | `1.00 TB` |

/// Decimal step between units.
const STEP: f64 = 1000.0;
/// Unit ladder, from bytes upwards.
const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
/// Decimals used for each entry of [`UNITS`].
const DECIMALS: [usize; 5] = [0, 1, 1, 1, 2];

/// Format `bytes` as a decimal, human-readable size.
///
/// ```
/// use broza_cli::output::bytes::format_bytes;
/// assert_eq!(format_bytes(512), "512 B");
/// assert_eq!(format_bytes(138_200_000_000), "138.2 GB");
/// assert_eq!(format_bytes(1_000_555_581_440), "1.00 TB");
/// ```
// Precision loss above 2^53 bytes (8 PB) is irrelevant: the result is rounded
// to at most two decimals of a terabyte anyway.
#[allow(clippy::cast_precision_loss)]
pub fn format_bytes(bytes: u64) -> String {
    let mut value = bytes as f64;
    let mut index = 0;
    while value >= STEP && index + 1 < UNITS.len() {
        value /= STEP;
        index += 1;
    }
    format!("{value:.*} {}", DECIMALS[index], UNITS[index])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn formats_the_examples_of_the_specification() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(138_200_000_000), "138.2 GB");
        assert_eq!(format_bytes(1_000_555_581_440), "1.00 TB");
    }

    #[test]
    fn zero_and_small_values_stay_in_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1), "1 B");
        assert_eq!(format_bytes(999), "999 B");
    }

    #[test]
    fn uses_decimal_steps_not_binary_ones() {
        assert_eq!(format_bytes(1_000), "1.0 KB");
        assert_eq!(format_bytes(1_024), "1.0 KB");
        assert_eq!(format_bytes(1_000_000), "1.0 MB");
        assert_eq!(format_bytes(1_000_000_000), "1.0 GB");
    }

    #[test]
    fn saturates_at_terabytes() {
        assert_eq!(format_bytes(u64::MAX), "18446744.07 TB");
    }

    #[test]
    fn rounds_to_the_documented_precision() {
        assert_eq!(format_bytes(84_140_000_000), "84.1 GB");
        assert_eq!(format_bytes(2_500_000_000_000), "2.50 TB");
    }
}
