//! `broza scan` arguments (`docs/cli-spec.md` §3.1).

use std::path::PathBuf;

use clap::Args;

/// Default depth of the folder tree shown.
///
/// Public because the command has to tell an explicit `--depth 2` from no
/// flag at all: until the folder walker lands, supplying one earns a note.
pub const DEFAULT_DEPTH: u32 = 2;
/// Default number of largest items listed. Public for the same reason.
pub const DEFAULT_TOP: u32 = 20;
/// Default minimum size of a listed item.
///
/// The one place where a command default deliberately differs from the
/// `min-size` configuration key (`docs/cli-spec.md` §3.1 vs §3.6): `scan`
/// lists consumers, `suggest` filters findings. Applied at use time, not by
/// clap, so the flag stays `None` when it is absent.
pub const SCAN_DEFAULT_MIN_SIZE: &str = "100MB";

/// Analyse storage and present the system map.
#[derive(Debug, Clone, Args)]
pub struct ScanArgs {
    /// Paths to analyse. Defaults to every mounted volume.
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,

    /// Depth of the folder tree shown.
    #[arg(long, value_name = "N", default_value_t = DEFAULT_DEPTH)]
    pub depth: u32,

    /// Number of largest items to list.
    #[arg(long, value_name = "N", default_value_t = DEFAULT_TOP)]
    pub top: u32,

    /// Ignore items below this size, for example 500MB, 2GB, 1GiB [default: 100MB].
    ///
    /// Unlike the other size flags this one does not read `min-size` from the
    /// configuration; `scan` and `suggest` filter different things.
    #[arg(long, value_name = "SIZE")]
    pub min_size: Option<String>,

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
