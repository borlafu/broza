//! `suggest` payload (`docs/cli-spec.md` §4.3).

use serde::{Deserialize, Serialize};

use crate::model::finding::{Finding, Risk};

/// Payload of `broza suggest`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SuggestReport {
    /// Sum of `reclaimable_bytes` over every finding.
    pub total_reclaimable_bytes: u64,
    /// The same sum, split by risk level.
    pub by_risk: RiskTotals,
    /// Findings, in the order the detectors produced them.
    #[serde(default)]
    pub findings: Vec<Finding>,
}

impl SuggestReport {
    /// Build a report whose totals are derived from `findings`.
    pub fn from_findings(findings: Vec<Finding>) -> Self {
        let by_risk = RiskTotals::from_findings(&findings);
        Self { total_reclaimable_bytes: by_risk.total(), by_risk, findings }
    }
}

/// Reclaimable bytes per risk level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RiskTotals {
    /// Bytes behind `green` findings.
    pub green: u64,
    /// Bytes behind `amber` findings.
    pub amber: u64,
    /// Bytes behind `red` findings. These are reported, never deleted by Broza.
    pub red: u64,
}

impl RiskTotals {
    /// Sum the reclaimable bytes of `findings` per risk level, saturating on overflow.
    pub fn from_findings(findings: &[Finding]) -> Self {
        findings.iter().fold(Self::default(), |totals, finding| {
            let bytes = finding.reclaimable_bytes();
            match finding.risk() {
                Risk::Green => Self { green: totals.green.saturating_add(bytes), ..totals },
                Risk::Amber => Self { amber: totals.amber.saturating_add(bytes), ..totals },
                Risk::Red => Self { red: totals.red.saturating_add(bytes), ..totals },
            }
        })
    }

    /// Sum of the three levels, saturating on overflow.
    pub const fn total(self) -> u64 {
        self.green.saturating_add(self.amber).saturating_add(self.red)
    }
}

#[cfg(test)]
mod tests {
    use super::{RiskTotals, SuggestReport};
    use crate::model::category::Category;
    use crate::model::finding::{Finding, Instructions, Risk};

    fn finding(id: &str, category: Category, risk: Risk, bytes: u64) -> Finding {
        let builder = Finding::builder(id.parse().unwrap_or_else(|e| panic!("{e}")), category, "t")
            .reclaimable_bytes(bytes);
        let builder = if category.is_inform_only() {
            builder.instructions(Instructions {
                provider: "iCloud Drive".into(),
                summary: "Use Apple's official feature.".into(),
                steps: vec!["System Settings".into()],
            })
        } else {
            builder.risk(risk)
        };
        builder.build().unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn totals_are_derived_per_risk_level() {
        let findings = vec![
            finding("user-cache.logs", Category::UserCache, Risk::Green, 100),
            finding("build-cache.xcode-deriveddata", Category::BuildCache, Risk::Green, 200),
            finding("duplicates.by-hash", Category::Duplicates, Risk::Amber, 50),
            finding("cloud-synced.icloud", Category::CloudSynced, Risk::Red, 25),
        ];
        let report = SuggestReport::from_findings(findings);
        assert_eq!(report.by_risk, RiskTotals { green: 300, amber: 50, red: 25 });
        assert_eq!(report.total_reclaimable_bytes, 375);
    }

    #[test]
    fn an_empty_report_has_zero_totals() {
        let report = SuggestReport::from_findings(Vec::new());
        assert_eq!(report.by_risk.total(), 0);
        assert_eq!(report.total_reclaimable_bytes, 0);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn totals_saturate_instead_of_overflowing() {
        let findings = vec![
            finding("user-cache.logs", Category::UserCache, Risk::Green, u64::MAX),
            finding("build-cache.node-modules", Category::BuildCache, Risk::Green, 1),
        ];
        assert_eq!(RiskTotals::from_findings(&findings).green, u64::MAX);
    }
}
