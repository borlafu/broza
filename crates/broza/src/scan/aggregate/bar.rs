//! The usage bar of the human output (`docs/cli-spec.md` §3.1).

/// Cell of a bar that is in use.
pub const BAR_FILLED: &str = "█";
/// Cell of a bar that is free.
pub const BAR_EMPTY: &str = "░";

/// Draw `used` out of `total` as a bar `width` cells wide.
///
/// Pure and total: an unknown total (`0`) draws an empty bar rather than dividing
/// by zero, and more used than there is draws a full one. Any non-zero usage keeps
/// at least one filled cell, because a bar that looks empty while the disk is not
/// is a lie the user acts on.
pub fn usage_bar(used: u64, total: u64, width: usize) -> String {
    let filled = filled_cells(used, total, width);
    format!("{}{}", BAR_FILLED.repeat(filled), BAR_EMPTY.repeat(width.saturating_sub(filled)))
}

/// How many cells of `width` are in use, rounded to the nearest cell.
fn filled_cells(used: u64, total: u64, width: usize) -> usize {
    if total == 0 || used == 0 || width == 0 {
        return 0;
    }
    if used >= total {
        return width;
    }
    let cells = u128::from(used) * width as u128;
    let rounded = (cells + u128::from(total) / 2) / u128::from(total);
    usize::try_from(rounded).unwrap_or(width).clamp(1, width)
}

#[cfg(test)]
mod tests {
    use super::{BAR_EMPTY, BAR_FILLED, usage_bar};

    fn bar(used: u64, total: u64, width: usize) -> String {
        usage_bar(used, total, width)
    }

    #[test]
    fn half_used_fills_half_the_bar() {
        assert_eq!(bar(50, 100, 10), format!("{}{}", BAR_FILLED.repeat(5), BAR_EMPTY.repeat(5)));
    }

    #[test]
    fn nothing_used_is_an_empty_bar_and_everything_used_is_a_full_one() {
        assert_eq!(bar(0, 100, 4), BAR_EMPTY.repeat(4));
        assert_eq!(bar(100, 100, 4), BAR_FILLED.repeat(4));
    }

    #[test]
    fn more_used_than_there_is_never_overflows_the_bar() {
        assert_eq!(bar(500, 100, 6), BAR_FILLED.repeat(6));
    }

    #[test]
    fn an_unknown_total_draws_an_empty_bar_instead_of_dividing_by_zero() {
        assert_eq!(bar(10, 0, 3), BAR_EMPTY.repeat(3));
    }

    #[test]
    fn a_fraction_of_a_cell_rounds_to_the_nearest_cell() {
        // 3 of 10 over 4 cells is 1.2 cells, and 7 of 10 is 2.8.
        assert_eq!(bar(3, 10, 4), format!("{BAR_FILLED}{}", BAR_EMPTY.repeat(3)));
        assert_eq!(bar(7, 10, 4), format!("{}{BAR_EMPTY}", BAR_FILLED.repeat(3)));
    }

    #[test]
    fn a_used_but_tiny_fraction_still_shows_one_cell() {
        assert_eq!(bar(1, 1_000_000, 20), format!("{}{}", BAR_FILLED, BAR_EMPTY.repeat(19)));
    }

    #[test]
    fn a_bar_without_width_is_empty() {
        assert_eq!(bar(1, 2, 0), "");
    }

    #[test]
    fn huge_numbers_do_not_overflow() {
        assert_eq!(bar(u64::MAX, u64::MAX, 2), BAR_FILLED.repeat(2));
    }
}
