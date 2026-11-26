//! `broza doctor` arguments (`docs/cli-spec.md` §3.9).

use clap::Args;

/// Check what Broza can read on this Mac and where it keeps its files.
#[derive(Debug, Clone, Default, Args)]
pub struct DoctorArgs {
    /// Open System Settings at Privacy & Security → Full Disk Access.
    #[arg(long)]
    pub open: bool,
}
