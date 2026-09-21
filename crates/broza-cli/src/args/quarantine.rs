//! `broza quarantine` arguments (`docs/cli-spec.md` §3.8).

use clap::{Args, Subcommand};

/// Manage the quarantine store.
#[derive(Debug, Clone, Args)]
pub struct QuarantineArgs {
    /// Operation to perform.
    #[command(subcommand)]
    pub command: QuarantineCommand,
}

/// Operations of `broza quarantine`.
#[derive(Debug, Clone, Subcommand)]
pub enum QuarantineCommand {
    /// List quarantine sessions.
    List,
    /// Permanently delete every session past its TTL.
    Expire {
        /// Skip the confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Permanently delete sessions regardless of TTL. Requires typing PURGE.
    Purge {
        /// Sessions to delete.
        #[arg(value_name = "SESSION_ID")]
        sessions: Vec<String>,
        /// Delete every session.
        #[arg(long)]
        all: bool,
        /// Accepted only to be rejected: purging never takes an implicit "yes"
        /// (`docs/cli-spec.md` §2 and AGENTS.md §2 invariant 2). Exits 2.
        #[arg(short = 'y', long)]
        yes: bool,
    },
}
