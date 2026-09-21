//! Output format resolution (`docs/cli-spec.md` §1.1).

/// Shape of the data written to stdout (or to `--output`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// Human-readable text. The default.
    #[default]
    Human,
    /// The JSON envelope of `docs/cli-spec.md` §4.
    Json,
    /// A flat CSV table, on tabular commands only.
    Csv,
}

impl OutputFormat {
    /// Resolve the format from the two mutually exclusive flags.
    ///
    /// The combination `--json --csv` is rejected earlier by
    /// [`crate::cli::Cli::validate`]; here `--json` simply wins.
    pub const fn resolve(json: bool, csv: bool) -> Self {
        if json {
            Self::Json
        } else if csv {
            Self::Csv
        } else {
            Self::Human
        }
    }

    /// `true` for formats consumed by programs, where color is never emitted.
    pub const fn is_machine_readable(self) -> bool {
        matches!(self, Self::Json | Self::Csv)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn no_flag_means_human_output() {
        assert_eq!(OutputFormat::resolve(false, false), OutputFormat::Human);
        assert!(!OutputFormat::Human.is_machine_readable());
    }

    #[test]
    fn each_flag_selects_its_format() {
        assert_eq!(OutputFormat::resolve(true, false), OutputFormat::Json);
        assert_eq!(OutputFormat::resolve(false, true), OutputFormat::Csv);
        assert!(OutputFormat::Json.is_machine_readable());
        assert!(OutputFormat::Csv.is_machine_readable());
    }
}
