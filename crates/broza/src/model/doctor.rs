//! `broza doctor` (`docs/cli-spec.md` §3.9, §4.8): what stands between Broza
//! and a complete scan, and where it keeps its own files.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Whether this process may read the folders macOS reserves to programs with
/// Full Disk Access.
///
/// A closed enum: only Broza produces it (see [`crate::model`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AccessState {
    /// A protected folder could be listed.
    Granted,
    /// macOS refused to list a protected folder.
    Missing,
    /// No protected folder Broza checks exists, or listing it failed for
    /// another reason; nothing can be concluded.
    Unknown,
}

/// What a check of Full Disk Access found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AccessCheck {
    /// The verdict.
    pub state: AccessState,
    /// The protected folder the verdict is based on; absent when none exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_path: Option<PathBuf>,
}

/// `data` payload of `broza doctor --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DoctorReport {
    /// Full Disk Access, as checked by listing a protected folder.
    pub full_disk_access: AccessCheck,
    /// The application to grant it to, when the terminal said which it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
    /// The link that opens the Full Disk Access pane of System Settings.
    pub settings_url: String,
    /// `true` when this run asked macOS to open that pane (`--open`).
    pub opened_settings: bool,
    /// The quarantine store; absent without a home directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine_path: Option<PathBuf>,
    /// The scan cache; absent without a home directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_path: Option<PathBuf>,
}
