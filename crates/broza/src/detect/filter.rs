//! Narrowing a detection report to what `suggest` was asked for.
//!
//! Pure functions over findings: no I/O. Filtering by category happens before
//! detection (a detector that is not wanted never runs); risk and size are
//! applied to the results here.

use crate::model::{Finding, Risk};

/// Which risk levels `--risk` keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RiskFilter {
    /// Every level (the default).
    #[default]
    All,
    /// Exactly this level.
    Only(Risk),
}

impl RiskFilter {
    /// `true` when a finding of `risk` passes the filter.
    pub const fn keeps(self, risk: Risk) -> bool {
        match self {
            Self::All => true,
            Self::Only(wanted) => matches!(
                (wanted, risk),
                (Risk::Green, Risk::Green) | (Risk::Amber, Risk::Amber) | (Risk::Red, Risk::Red)
            ),
        }
    }
}

/// Keep the findings that pass `--risk` and are at least `min_size` bytes.
///
/// Inform-only findings are kept whatever their size: they are not reclaimable
/// space but a fact the user asked to know, and hiding them by size would make
/// the cloud-synced explanation appear and disappear from one run to the next.
/// So are findings whose size macOS does not report (the snapshots): their `0`
/// means "unknown", and a size filter has nothing to compare it with.
pub fn apply(findings: Vec<Finding>, risk: RiskFilter, min_size: u64) -> Vec<Finding> {
    findings
        .into_iter()
        .filter(|finding| risk.keeps(finding.risk()))
        .filter(|finding| {
            !finding.is_actionable() || finding.size_is_unknown() || finding.reclaimable_bytes() >= min_size
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Action, Category, FindingId, FindingPath};

    fn finding(id: &str, category: Category, bytes: u64) -> Finding {
        let id: FindingId = id.parse().unwrap_or_else(|e| panic!("{e}"));
        let mut builder = Finding::builder(id, category, "t").reclaimable_bytes(bytes).item_count(1);
        if category.is_inform_only() {
            builder = builder.action(Action::InformOnly).instructions(crate::model::Instructions {
                provider: "p".into(),
                summary: "s".into(),
                steps: Vec::new(),
            });
        } else {
            builder = builder.paths(vec![FindingPath {
                path: std::path::PathBuf::from("/Users/dana/x"),
                size_bytes: bytes,
                last_used: None,
            }]);
        }
        builder.build().unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn the_risk_filter_keeps_exactly_the_level_asked_for() {
        let all = vec![
            finding("user-cache.a", Category::UserCache, 100),
            finding("trash.b", Category::Trash, 100),
            finding("cloud-synced.c", Category::CloudSynced, 100),
        ];

        let green = apply(all.clone(), RiskFilter::Only(Risk::Green), 0);
        let red = apply(all.clone(), RiskFilter::Only(Risk::Red), 0);
        let everything = apply(all, RiskFilter::All, 0);

        assert_eq!(green.iter().map(|f| f.id().to_string()).collect::<Vec<_>>(), vec!["user-cache.a"]);
        assert_eq!(red.iter().map(|f| f.id().to_string()).collect::<Vec<_>>(), vec!["cloud-synced.c"]);
        assert_eq!(everything.len(), 3);
    }

    #[test]
    fn a_finding_whose_size_macos_does_not_report_survives_the_size_filter() {
        let snapshots = Finding::builder(
            "snapshots.timemachine-local".parse().unwrap_or_else(|e| panic!("{e}")),
            Category::Snapshots,
            "t",
        )
        .item_count(4)
        .reasoning("size not reported by macOS")
        .build()
        .unwrap_or_else(|e| panic!("{e}"));

        let kept = apply(vec![snapshots], RiskFilter::All, 50_000_000);

        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn small_findings_are_dropped_but_inform_only_ones_never_are() {
        let all = vec![
            finding("user-cache.small", Category::UserCache, 10),
            finding("user-cache.big", Category::UserCache, 1000),
            finding("cloud-synced.tiny", Category::CloudSynced, 1),
        ];

        let kept = apply(all, RiskFilter::All, 100);

        let ids: Vec<String> = kept.iter().map(|f| f.id().to_string()).collect();
        assert_eq!(ids, vec!["user-cache.big", "cloud-synced.tiny"]);
    }
}
