//! `broza scan` arguments (`docs/cli-spec.md` §3.1).

use std::path::PathBuf;

use clap::Args;

/// Default depth of the folder tree shown.
const DEFAULT_DEPTH: &str = "2";
/// Default number of largest items listed.
const DEFAULT_TOP: &str = "20";
/// Default minimum size of a listed item.
const DEFAULT_MIN_SIZE: &str = "100MB";

/// Analyse storage and present the system map.
#[derive(Debug, Clone, Args)]
pub struct ScanArgs {
    /// Paths to analyse. Defaults to every mounted volume.
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,

    /// Depth of the folder tree shown.
    #[arg(long, value_name = "N", default_value = DEFAULT_DEPTH)]
    pub depth: u32,

    /// Number of largest items to list.
    #[arg(long, value_name = "N", default_value = DEFAULT_TOP)]
    pub top: u32,

    /// Ignore items below this size (for example 500MB, 2GB, 1GiB).
    #[arg(long, value_name = "SIZE", default_value = DEFAULT_MIN_SIZE)]
    pub min_size: String,

    /// Restrict the scan to one volume (device id, name or mount point).
    #[arg(long, value_name = "ID")]
    pub volume: Option<String>,

    /// Exclude mounted external disks.
    #[arg(long)]
    pub no_external: bool,

    /// Force the hierarchical tree view.
    #[arg(long)]
    pub tree: bool,
}
