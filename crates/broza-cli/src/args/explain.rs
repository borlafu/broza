//! `broza explain` arguments (`docs/cli-spec.md` §3.2).

use clap::Args;

/// Explain what a volume, path or cleanup category is.
#[derive(Debug, Clone, Args)]
pub struct ExplainArgs {
    /// Category id, volume (device id, name or mount point) or filesystem path.
    #[arg(value_name = "PATH|VOLUME|CATEGORY")]
    pub target: String,

    /// One-line summary only.
    #[arg(long)]
    pub short: bool,
}
