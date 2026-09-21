//! `broza clean` arguments (`docs/cli-spec.md` §3.4).

use clap::Args;

use crate::args::RiskLevel;

/// Default "unused app" threshold.
const DEFAULT_UNUSED_AFTER: &str = "1y";

/// Execute the cleanup. Dry run by default.
#[derive(Debug, Clone, Args)]
pub struct CleanArgs {
    /// Required to modify the disk. Without it, only simulates.
    #[arg(long)]
    pub apply: bool,

    /// Categories to clean. Repeatable or comma-separated. Mandatory unless --risk is used.
    #[arg(long = "category", value_name = "ID", value_delimiter = ',')]
    pub categories: Vec<String>,

    /// Clean everything at this risk level or lower.
    #[arg(long, value_name = "LEVEL")]
    pub risk: Option<RiskLevel>,

    /// Irreversible deletion, bypassing quarantine. Requires typing PURGE.
    #[arg(long)]
    pub purge: bool,

    /// Assume "yes" on confirmations. Never allowed together with --purge.
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Safety cap: abort if more than this amount would be removed.
    #[arg(long, value_name = "SIZE")]
    pub max_size: Option<String>,

    /// Glob patterns to exclude. Repeatable; merged with `exclude` from the configuration.
    #[arg(long, value_name = "GLOB")]
    pub exclude: Vec<String>,

    /// Threshold for unused-apps.
    #[arg(long, value_name = "DURATION", default_value = DEFAULT_UNUSED_AFTER)]
    pub unused_after: String,
}
