//! `broza config` arguments (`docs/cli-spec.md` §3.6).

use clap::{Args, Subcommand};

/// Read and write configuration keys.
#[derive(Debug, Clone, Args)]
pub struct ConfigArgs {
    /// Operation to perform.
    #[command(subcommand)]
    pub command: ConfigCommand,
}

/// Operations of `broza config`.
#[derive(Debug, Clone, Subcommand)]
pub enum ConfigCommand {
    /// Print the value of one key.
    Get {
        /// Configuration key, for example `min-size`.
        key: String,
    },
    /// Validate a value and write it to the configuration file.
    Set {
        /// Configuration key, for example `min-size`.
        key: String,
        /// New value; validated against the key's type.
        value: String,
    },
    /// Print every key with its effective value.
    List,
    /// Print the path of the configuration file in use.
    Path,
    /// Restore one key, or every key, to its default.
    Reset {
        /// Configuration key. Omit to reset everything.
        key: Option<String>,
    },
}
