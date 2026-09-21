//! `broza restore` arguments (`docs/cli-spec.md` §3.5).

use std::path::PathBuf;

use clap::Args;

/// Recover items from quarantine.
#[derive(Debug, Clone, Args)]
pub struct RestoreArgs {
    /// Session id (`cln_…`) or item id (`cln_…/<seq>`).
    #[arg(value_name = "ID")]
    pub ids: Vec<String>,

    /// List quarantine contents without restoring.
    #[arg(long)]
    pub list: bool,

    /// Restore everything currently in quarantine.
    #[arg(long, conflicts_with_all = ["ids", "session"])]
    pub all: bool,

    /// Restore one complete cleanup session.
    #[arg(long, value_name = "ID", conflicts_with = "ids")]
    pub session: Option<String>,

    /// Restore to an alternative location instead of the original path.
    #[arg(long, value_name = "PATH")]
    pub to: Option<PathBuf>,
}
