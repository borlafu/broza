//! `broza explain` arguments (`docs/cli-spec.md` §3.2).

use clap::Args;

/// Explain what a volume, path or cleanup category is.
#[derive(Debug, Clone, Args)]
pub struct ExplainArgs {
    /// Category id, volume (device id, name or mount point) or filesystem path.
    #[arg(value_name = "PATH|VOLUME|CATEGORY")]
    pub target: String,

    /// With a category: a finding Broza only reports (for example icloud, docker-raw,
    /// unverified); prints the steps that free its space.
    #[arg(value_name = "FINDING")]
    pub finding: Option<String>,

    /// One-line summary only.
    #[arg(long)]
    pub short: bool,
}
