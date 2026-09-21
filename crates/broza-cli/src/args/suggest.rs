//! `broza suggest` arguments (`docs/cli-spec.md` §3.3).

use clap::Args;

use crate::args::RiskFilter;

/// Detect cleanable categories. Never writes anything.
#[derive(Debug, Clone, Args)]
pub struct SuggestArgs {
    /// Restrict to specific categories. Repeatable or comma-separated.
    #[arg(long = "category", value_name = "ID", value_delimiter = ',')]
    pub categories: Vec<String>,

    /// Filter by risk level.
    #[arg(long, value_name = "LEVEL", default_value = "all")]
    pub risk: RiskFilter,

    /// Omit findings smaller than this size [default: the `min-size` key, 50MB].
    #[arg(long, value_name = "SIZE")]
    pub min_size: Option<String>,

    /// "Unused app" threshold, for example 6m, 1y, 2y [default: the `unused-after` key, 1y].
    #[arg(long, value_name = "DURATION")]
    pub unused_after: Option<String>,

    /// Include the reasoning behind each detection.
    #[arg(long)]
    pub explain: bool,
}
