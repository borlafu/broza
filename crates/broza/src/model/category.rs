//! Detection categories and their default risk and action (`docs/cli-spec.md` §3.3).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::BrozaError;
use crate::model::finding::{Action, Risk};

/// A detection category. The identifiers are stable and usable in scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Category {
    /// `~/Library/Caches`, logs and incomplete downloads.
    UserCache,
    /// Build artefacts: `DerivedData`, archives, orphan `node_modules`, `target/`.
    BuildCache,
    /// Unused iOS simulator runtimes and devices.
    IosSimulators,
    /// Trash folders on every volume.
    Trash,
    /// APFS local snapshots taken by Time Machine.
    Snapshots,
    /// iOS device backups under `MobileSync/Backup`.
    OldBackups,
    /// Applications not opened past the threshold, plus their leftovers.
    UnusedApps,
    /// Files already synced to a cloud provider. Broza never deletes these.
    CloudSynced,
    /// Identical copies confirmed by content hash.
    Duplicates,
    /// Large files not opened for a long time.
    LargeOldFiles,
}

/// Every category, in the order of the table in `docs/cli-spec.md` §3.3.
pub const ALL_CATEGORIES: [Category; 10] = [
    Category::UserCache,
    Category::BuildCache,
    Category::IosSimulators,
    Category::Trash,
    Category::Snapshots,
    Category::OldBackups,
    Category::UnusedApps,
    Category::CloudSynced,
    Category::Duplicates,
    Category::LargeOldFiles,
];

impl Category {
    /// Every category, in specification order.
    pub const fn all() -> [Self; 10] {
        ALL_CATEGORIES
    }

    /// The stable kebab-case identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserCache => "user-cache",
            Self::BuildCache => "build-cache",
            Self::IosSimulators => "ios-simulators",
            Self::Trash => "trash",
            Self::Snapshots => "snapshots",
            Self::OldBackups => "old-backups",
            Self::UnusedApps => "unused-apps",
            Self::CloudSynced => "cloud-synced",
            Self::Duplicates => "duplicates",
            Self::LargeOldFiles => "large-old-files",
        }
    }

    /// Action proposed by default for findings of this category.
    ///
    /// Individual detectors may lower a finding to [`Action::InformOnly`] (the Docker
    /// virtual disk, for example), but never the other way round.
    pub const fn default_action(self) -> Action {
        match self {
            Self::Trash => Action::Purge,
            Self::Snapshots => Action::TmutilDelete,
            Self::CloudSynced => Action::InformOnly,
            Self::UserCache
            | Self::BuildCache
            | Self::IosSimulators
            | Self::OldBackups
            | Self::UnusedApps
            | Self::Duplicates
            | Self::LargeOldFiles => Action::Quarantine,
        }
    }

    /// Lowest risk a finding of this category can carry.
    ///
    /// Categories documented as `green / amber` or `amber / red` use the lower value
    /// here; a detector raises it per finding when it has a reason to.
    pub const fn base_risk(self) -> Risk {
        match self {
            Self::UserCache | Self::BuildCache => Risk::Green,
            Self::CloudSynced => Risk::Red,
            Self::IosSimulators
            | Self::Trash
            | Self::Snapshots
            | Self::OldBackups
            | Self::UnusedApps
            | Self::Duplicates
            | Self::LargeOldFiles => Risk::Amber,
        }
    }

    /// `true` when Broza is never allowed to delete findings of this category
    /// (`AGENTS.md` §2.5).
    pub const fn is_inform_only(self) -> bool {
        matches!(self, Self::CloudSynced)
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Category {
    type Err = BrozaError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        ALL_CATEGORIES
            .into_iter()
            .find(|category| category.as_str() == raw)
            .ok_or_else(|| BrozaError::Usage(format!("unknown category `{raw}`")))
    }
}

#[cfg(test)]
mod tests {
    use super::Category;
    use crate::model::finding::{Action, Risk};
    use std::str::FromStr;

    #[test]
    fn the_specification_table_is_reproduced_exactly() {
        let table = [
            (Category::UserCache, "user-cache", Risk::Green, Action::Quarantine),
            (Category::BuildCache, "build-cache", Risk::Green, Action::Quarantine),
            (Category::IosSimulators, "ios-simulators", Risk::Amber, Action::Quarantine),
            (Category::Trash, "trash", Risk::Amber, Action::Purge),
            (Category::Snapshots, "snapshots", Risk::Amber, Action::TmutilDelete),
            (Category::OldBackups, "old-backups", Risk::Amber, Action::Quarantine),
            (Category::UnusedApps, "unused-apps", Risk::Amber, Action::Quarantine),
            (Category::CloudSynced, "cloud-synced", Risk::Red, Action::InformOnly),
            (Category::Duplicates, "duplicates", Risk::Amber, Action::Quarantine),
            (Category::LargeOldFiles, "large-old-files", Risk::Amber, Action::Quarantine),
        ];
        assert_eq!(table.len(), Category::all().len());
        for (category, id, risk, action) in table {
            assert_eq!(category.as_str(), id);
            assert_eq!(category.to_string(), id);
            assert_eq!(category.base_risk(), risk, "{id}");
            assert_eq!(category.default_action(), action, "{id}");
            assert_eq!(Category::from_str(id).unwrap_or_else(|e| panic!("{e}")), category);
            let json = serde_json::to_string(&category).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(json, format!("\"{id}\""));
        }
    }

    #[test]
    fn only_cloud_synced_is_inform_only() {
        for category in Category::all() {
            assert_eq!(category.is_inform_only(), category == Category::CloudSynced, "{category}");
        }
    }

    #[test]
    fn unknown_category_identifiers_are_rejected() {
        for raw in ["", "UserCache", "user_cache", "downloads"] {
            assert!(Category::from_str(raw).is_err(), "{raw:?}");
            assert!(serde_json::from_str::<Category>(&format!("\"{raw}\"")).is_err(), "{raw:?}");
        }
    }
}
