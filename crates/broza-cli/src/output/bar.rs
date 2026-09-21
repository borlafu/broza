//! The usage bar of `broza scan` (`docs/cli-spec.md` §3.1).
//!
//! A pure function of two numbers. The percentage it draws is always a
//! *container* percentage: APFS volumes share the space of their container, so
//! a per-volume bar would add up to several hundred per cent on a normal Mac.
//!
//! The bar is decoration. Every line that carries one also carries the
//! percentage as text, because color and glyphs are never the only channel
//! (RNF-06).

/// Cells in a bar.
pub const BAR_WIDTH: usize = 20;
/// Cell of a used bar.
const FULL_CELL: char = '█';
/// Cell of a free bar.
const EMPTY_CELL: char = '░';
/// At or above this share of the container, the bar warns.
pub const BUSY_RATIO: f64 = 0.80;
/// At or above this share of the container, the bar alarms.
pub const FULL_RATIO: f64 = 0.90;

/// How full a container is, as a fraction between 0 and 1.
///
/// An empty or unreported container is 0, never a division by zero, and a
/// container reporting more used than it has is clamped to 1 rather than
/// drawing a bar wider than its own row.
// A container is at most a few petabytes; `f64` represents that exactly enough
// for a twenty-cell bar.
#[allow(clippy::cast_precision_loss)]
pub fn usage_ratio(used_bytes: u64, size_bytes: u64) -> f64 {
    if size_bytes == 0 {
        return 0.0;
    }
    (used_bytes as f64 / size_bytes as f64).clamp(0.0, 1.0)
}

/// A [`BAR_WIDTH`]-cell bar for `used_bytes` out of `size_bytes`.
///
/// ```
/// use broza_cli::output::bar::usage_bar;
/// assert_eq!(usage_bar(0, 100).chars().count(), 20);
/// assert_eq!(usage_bar(50, 100), "██████████░░░░░░░░░░");
/// ```
// The ratio is in `[0, 1]` and the width is 20, so the product is a small
// non-negative number: neither cast can truncate meaningfully or wrap.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
pub fn usage_bar(used_bytes: u64, size_bytes: u64) -> String {
    let filled = (usage_ratio(used_bytes, size_bytes) * BAR_WIDTH as f64).round() as usize;
    let filled = filled.min(BAR_WIDTH);
    let mut bar = String::with_capacity(BAR_WIDTH * FULL_CELL.len_utf8());
    bar.extend(std::iter::repeat_n(FULL_CELL, filled));
    bar.extend(std::iter::repeat_n(EMPTY_CELL, BAR_WIDTH - filled));
    bar
}

/// The percentage text printed next to the bar, one decimal.
///
/// ```
/// use broza_cli::output::bar::usage_percentage;
/// assert_eq!(usage_percentage(812_400_000_000, 994_662_584_320), "81.7%");
/// ```
pub fn usage_percentage(used_bytes: u64, size_bytes: u64) -> String {
    format!("{:.1}%", usage_ratio(used_bytes, size_bytes) * 100.0)
}

/// How alarming a fill level is, for the color the bar is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fullness {
    /// Below [`BUSY_RATIO`]: nothing to say.
    Comfortable,
    /// Between [`BUSY_RATIO`] and [`FULL_RATIO`].
    Busy,
    /// At or above [`FULL_RATIO`].
    Full,
}

/// Classify the fill level of a container.
pub fn fullness(used_bytes: u64, size_bytes: u64) -> Fullness {
    let ratio = usage_ratio(used_bytes, size_bytes);
    if ratio >= FULL_RATIO {
        Fullness::Full
    } else if ratio >= BUSY_RATIO {
        Fullness::Busy
    } else {
        Fullness::Comfortable
    }
}

#[cfg(test)]
mod tests {
    // The ratios compared here are 0.0 and 1.0, which `f64` represents exactly.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]

    use super::*;

    #[test]
    fn a_bar_is_always_exactly_twenty_cells() {
        for (used, size) in [(0, 0), (0, 100), (1, 100), (99, 100), (100, 100), (200, 100)] {
            assert_eq!(usage_bar(used, size).chars().count(), BAR_WIDTH, "{used}/{size}");
        }
    }

    #[test]
    fn the_sketch_of_the_specification_is_reproduced() {
        // `docs/cli-spec.md` §3.1: 812.40 GB of a 994.66 GB container.
        assert_eq!(usage_percentage(812_400_000_000, 994_662_584_320), "81.7%");
        assert_eq!(usage_bar(812_400_000_000, 994_662_584_320), "████████████████░░░░");
    }

    #[test]
    fn an_empty_container_never_divides_by_zero() {
        assert_eq!(usage_ratio(10, 0), 0.0);
        assert_eq!(usage_percentage(10, 0), "0.0%");
        assert_eq!(usage_bar(10, 0), "░".repeat(BAR_WIDTH));
        assert_eq!(fullness(10, 0), Fullness::Comfortable);
    }

    #[test]
    fn more_used_than_capacity_is_clamped_instead_of_overflowing_the_row() {
        assert_eq!(usage_ratio(300, 100), 1.0);
        assert_eq!(usage_bar(300, 100), "█".repeat(BAR_WIDTH));
        assert_eq!(usage_percentage(300, 100), "100.0%");
    }

    #[test]
    fn the_thresholds_are_the_documented_ones() {
        assert_eq!(fullness(79, 100), Fullness::Comfortable);
        assert_eq!(fullness(80, 100), Fullness::Busy);
        assert_eq!(fullness(89, 100), Fullness::Busy);
        assert_eq!(fullness(90, 100), Fullness::Full);
        assert_eq!(fullness(100, 100), Fullness::Full);
    }

    #[test]
    fn a_nearly_empty_container_still_shows_an_empty_bar() {
        assert_eq!(usage_bar(1, 1_000_000), "░".repeat(BAR_WIDTH));
        assert_eq!(usage_percentage(1, 1_000_000), "0.0%");
    }
}
