//! Paragraph wrapping for `broza explain` (`docs/cli-spec.md` §3.2).
//!
//! A fixed width rather than the terminal's: the output is snapshot-tested and
//! often piped, and a paragraph that reflows with the window is a paragraph
//! nobody can diff. Words longer than the width — a path, a bundle id — are
//! never broken; a line is allowed to overflow instead, because a cut path is
//! worse than a ragged margin.

/// Width, in characters, of a wrapped paragraph including its indent.
pub const WRAP_WIDTH: usize = 78;

/// Wrap `text` to [`WRAP_WIDTH`], prefixing every line with `indent`.
///
/// ```
/// use broza_cli::output::wrap::wrap;
/// assert_eq!(wrap("one two", "  "), "  one two");
/// ```
pub fn wrap(text: &str, indent: &str) -> String {
    wrap_to(text, indent, WRAP_WIDTH)
}

/// [`wrap`] to an explicit `width`; the seam the tests use.
pub fn wrap_to(text: &str, indent: &str, width: usize) -> String {
    let budget = width.saturating_sub(indent.chars().count()).max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let extra = usize::from(!current.is_empty());
        if !current.is_empty() && current.chars().count() + extra + word.chars().count() > budget {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines.iter().map(|line| format!("{indent}{line}")).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_short_paragraph_stays_on_one_indented_line() {
        assert_eq!(wrap("The data volume.", "  "), "  The data volume.");
    }

    #[test]
    fn a_long_paragraph_is_broken_at_word_boundaries() {
        let wrapped = wrap_to("aaa bbb ccc ddd", "", 7);
        assert_eq!(wrapped, "aaa bbb\nccc ddd");
    }

    #[test]
    fn no_line_exceeds_the_width_when_the_words_fit() {
        let text = "The mutable data volume of macOS. It holds /Users, the applications you \
install, your user libraries and caches.";
        for line in wrap(text, "  ").lines() {
            assert!(line.chars().count() <= WRAP_WIDTH, "{} chars: {line}", line.chars().count());
        }
    }

    #[test]
    fn every_line_carries_the_indent() {
        let wrapped = wrap_to("aaa bbb ccc ddd", ">>", 7);
        assert!(wrapped.lines().all(|line| line.starts_with(">>")), "{wrapped}");
    }

    #[test]
    fn a_word_longer_than_the_width_is_never_cut() {
        let long = "/Users/dana/Library/Containers/com.example.application/Data";
        let wrapped = wrap_to(long, "  ", 20);
        assert_eq!(wrapped, format!("  {long}"));
    }

    #[test]
    fn existing_whitespace_is_normalised() {
        assert_eq!(wrap("one\n\n  two\tthree", ""), "one two three");
    }

    #[test]
    fn empty_text_produces_no_line_at_all() {
        assert_eq!(wrap("", "  "), "");
        assert_eq!(wrap("   \n ", "  "), "");
    }
}
