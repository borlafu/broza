//! `broza suggest` arguments (`docs/cli-spec.md` §3.3).

use clap::Args;

use crate::args::RiskFilter;

/// Default minimum size of a reported finding.
const DEFAULT_MIN_SIZE: &str = "50MB";
/// Default "unused app" threshold.
const DEFAULT_UNUSED_AFTER: &str = "1y";

/// Detect cleanable categories. Never writes anything.
#[derive(Debug, Clone, Args)]
pub struct SuggestArgs {
    /// Restrict to specific categories. Repeatable or comma-separated.
    #[arg(long = "category", value_name = "ID", value_delimiter = ',')]
    pub categories: Vec<String>,

    /// Filter by risk level.
    #[arg(long, value_name = "LEVEL", default_value = "all")]
    pub risk: RiskFilter,

    /// Omit findings smaller than this size.
    #[arg(long, value_name = "SIZE", default_value = DEFAULT_MIN_SIZE)]
    pub min_size: String,

    /// "Unused app" threshold (for example 6m, 1y, 2y).
    #[arg(long, value_name = "DURATION", default_value = DEFAULT_UNUSED_AFTER)]
    pub unused_after: String,

    /// Include the reasoning behind each detection.
    #[arg(long)]
    pub explain: bool,
}
