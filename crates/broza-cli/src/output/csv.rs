//! Flat CSV rows for the tabular commands (`docs/cli-spec.md` §1.1).
//!
//! RFC 4180 with the one deviation every shell expects: no trailing `\r`.
//! A field is quoted only when it has to be, so a table of identifiers and
//! numbers stays readable, and a volume called `Photos, 2019` still parses.

/// Separator between fields.
const SEPARATOR: char = ',';
/// Quote character, doubled inside a quoted field.
const QUOTE: char = '"';
/// Characters that force a field to be quoted.
const MUST_QUOTE: [char; 4] = [',', '"', '\n', '\r'];

/// One CSV line, without its line break.
///
/// ```
/// use broza_cli::output::csv::row;
/// assert_eq!(row(&["disk3s5", "Macintosh HD - Data"]), "disk3s5,Macintosh HD - Data");
/// assert_eq!(row(&["a,b"]), "\"a,b\"");
/// ```
pub fn row<T: AsRef<str>>(fields: &[T]) -> String {
    fields.iter().map(|field| field_of(field.as_ref())).collect::<Vec<_>>().join(&SEPARATOR.to_string())
}

/// One field, quoted only when its content requires it.
fn field_of(field: &str) -> String {
    if !field.contains(MUST_QUOTE) {
        return field.to_owned();
    }
    let escaped = field.replace(QUOTE, "\"\"");
    format!("{QUOTE}{escaped}{QUOTE}")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn plain_fields_are_left_alone() {
        assert_eq!(row(&["disk0", "disk3", "1000"]), "disk0,disk3,1000");
    }

    #[test]
    fn a_field_with_a_separator_is_quoted() {
        assert_eq!(field_of("Photos, 2019"), "\"Photos, 2019\"");
    }

    #[test]
    fn a_quote_inside_a_field_is_doubled() {
        assert_eq!(field_of("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn a_newline_forces_quoting_so_a_row_stays_one_record() {
        assert_eq!(field_of("a\nb"), "\"a\nb\"");
        assert_eq!(field_of("a\rb"), "\"a\rb\"");
    }

    #[test]
    fn an_empty_row_and_empty_fields_are_representable() {
        let empty: [&str; 0] = [];
        assert_eq!(row(&empty), "");
        assert_eq!(row(&["", ""]), ",");
    }

    #[test]
    fn rows_accept_owned_strings_too() {
        assert_eq!(row(&["a".to_owned(), "b".to_owned()]), "a,b");
    }
}
