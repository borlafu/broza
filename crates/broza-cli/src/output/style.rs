//! ANSI styling, applied only where [`ColorPolicy`] allows it.
//!
//! Color is never the only channel (RNF-06): every role, risk and fill level
//! this module paints is also spelled out in words on the same line. Turning
//! color off therefore removes decoration and no information, which is what
//! the tests below pin.

use crate::output::color::ColorPolicy;

/// Escape that returns the terminal to its own colors.
const RESET: &str = "\u{1b}[0m";

/// The handful of styles Broza's human output uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// Emphasis: the user's own data.
    Bold,
    /// De-emphasis: volumes Broza will never write to.
    Dim,
    /// A comfortable fill level.
    Green,
    /// A fill level worth noticing.
    Yellow,
    /// A fill level worth acting on.
    Red,
}

impl Style {
    /// The escape that starts this style.
    const fn escape(self) -> &'static str {
        match self {
            Self::Bold => "\u{1b}[1m",
            Self::Dim => "\u{1b}[2m",
            Self::Green => "\u{1b}[32m",
            Self::Yellow => "\u{1b}[33m",
            Self::Red => "\u{1b}[31m",
        }
    }
}

/// `text` in `style`, or `text` unchanged when `policy` forbids color.
pub fn paint(policy: ColorPolicy, style: Style, text: &str) -> String {
    if !policy.is_enabled() || text.is_empty() {
        return text.to_owned();
    }
    format!("{}{text}{RESET}", style.escape())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_disabled_policy_returns_the_text_untouched() {
        for style in [Style::Bold, Style::Dim, Style::Green, Style::Yellow, Style::Red] {
            assert_eq!(paint(ColorPolicy::Never, style, "Data"), "Data");
        }
    }

    #[test]
    fn an_enabled_policy_wraps_the_text_and_always_resets() {
        let painted = paint(ColorPolicy::Auto, Style::Bold, "Data");

        assert!(painted.starts_with("\u{1b}[1m"), "{painted:?}");
        assert!(painted.ends_with(RESET), "{painted:?}");
        assert!(painted.contains("Data"));
    }

    #[test]
    fn styling_never_changes_the_visible_characters() {
        let painted = paint(ColorPolicy::Always, Style::Dim, "Preboot");
        let stripped = painted.replace(Style::Dim.escape(), "").replace(RESET, "");
        assert_eq!(stripped, "Preboot");
    }

    #[test]
    fn empty_text_is_never_wrapped_in_escapes() {
        assert_eq!(paint(ColorPolicy::Always, Style::Red, ""), "");
    }

    #[test]
    fn every_style_has_its_own_escape() {
        let mut escapes =
            [Style::Bold, Style::Dim, Style::Green, Style::Yellow, Style::Red].map(Style::escape).to_vec();
        escapes.sort_unstable();
        let total = escapes.len();
        escapes.dedup();
        assert_eq!(escapes.len(), total);
    }
}
