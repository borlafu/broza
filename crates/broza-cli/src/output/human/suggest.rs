//! The human rendering of `broza suggest` (`docs/cli-spec.md` §3.3).
//!
//! Findings are grouped by risk, each group headed by its text label and its
//! total, then by category. Every risk carries a word as well as a colour
//! (RNF-06), inform-only findings say in the same breath that Broza does not
//! delete them, and the footer names the two commands that come next — the dry
//! run first.

use std::fmt::Write as _;
use std::path::Path;

use broza::model::{Category, Finding, Risk, SuggestReport};

use crate::output::human::risk_chip;
use crate::output::human::scan::folders::abbreviate;
use crate::output::{ColorPolicy, format_bytes};

/// Width of the category column.
const CATEGORY_WIDTH: usize = 16;
/// Width of the size column.
const SIZE_WIDTH: usize = 9;
/// What is printed when no detector found anything.
const NOTHING_FOUND: &str = "Nothing to suggest: no cleanable category reached --min-size.";

/// Render `report` for a terminal.
pub fn render(report: &SuggestReport, explain: bool, home: &Path, policy: ColorPolicy) -> String {
    if report.findings.is_empty() {
        return NOTHING_FOUND.to_owned();
    }
    let mut text =
        format!("Potentially reclaimable space:  {}", format_bytes(report.total_reclaimable_bytes));
    for (risk, total) in [
        (Risk::Green, report.by_risk.green),
        (Risk::Amber, report.by_risk.amber),
        (Risk::Red, report.by_risk.red),
    ] {
        let findings: Vec<&Finding> = report.findings.iter().filter(|f| f.risk() == risk).collect();
        if findings.is_empty() {
            continue;
        }
        let _ignored = write!(text, "\n\n{} — {}", heading(risk, policy), format_bytes(total));
        for category in Category::all() {
            let of_category: Vec<&Finding> =
                findings.iter().copied().filter(|f| f.category() == category).collect();
            if !of_category.is_empty() {
                render_category(&mut text, category, &of_category, explain, home);
            }
        }
    }
    let _ignored = write!(text, "\n\n{}", footer());
    text
}

/// `SAFE (green)` and friends, with the label painted and the token in words.
fn heading(risk: Risk, policy: ColorPolicy) -> String {
    let token = match risk {
        Risk::Green => "green",
        Risk::Amber => "amber",
        _ => "red",
    };
    format!("{} ({token})", risk_chip(policy, risk))
}

/// One category line, with its findings summarised, plus their details.
fn render_category(text: &mut String, category: Category, findings: &[&Finding], explain: bool, home: &Path) {
    let total: u64 = findings.iter().map(|f| f.reclaimable_bytes()).fold(0, u64::saturating_add);
    let summary = findings
        .iter()
        .map(|f| format!("{} ({})", f.title(), format_bytes(f.reclaimable_bytes())))
        .collect::<Vec<_>>()
        .join(", ");
    let _ignored = write!(
        text,
        "\n   {:<CATEGORY_WIDTH$}{:>SIZE_WIDTH$}   {summary}",
        category.as_str(),
        format_bytes(total)
    );
    for finding in findings {
        if !finding.is_actionable() {
            let _ignored = write!(
                text,
                "\n   {:<CATEGORY_WIDTH$}{:>SIZE_WIDTH$}   → Broza does not delete this. See: broza explain {}",
                "",
                "",
                category.as_str()
            );
        }
        if explain {
            render_reasoning(text, finding, home);
        }
    }
}

/// `--explain`: the reasoning and the paths behind one finding.
fn render_reasoning(text: &mut String, finding: &Finding, home: &Path) {
    let indent = " ".repeat(3 + CATEGORY_WIDTH + SIZE_WIDTH + 3);
    if let Some(reasoning) = finding.reasoning() {
        let _ignored = write!(text, "\n{indent}{}: {reasoning}", finding.id());
    }
    for path in finding.paths().iter().take(5) {
        let _ignored = write!(
            text,
            "\n{indent}  {:>SIZE_WIDTH$}  {}",
            format_bytes(path.size_bytes),
            abbreviate(&path.path, Some(home))
        );
    }
    if finding.paths().len() > 5 {
        let _ignored = write!(text, "\n{indent}  … and {} more", finding.paths().len() - 5);
    }
}

/// What to run next: the dry run, then the real thing.
fn footer() -> String {
    "Next step:\n  broza clean --risk green            (dry run, deletes nothing)\n  broza clean --risk green --apply    (moves to quarantine; space is freed after expiry or purge)".to_owned()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::PathBuf;

    use broza::model::{Action, FindingId, FindingPath, Instructions};

    use super::*;

    fn finding(id: &str, category: Category, bytes: u64, inform: bool) -> Finding {
        let id: FindingId = id.parse().unwrap();
        let mut b = Finding::builder(id, category, format!("Title of {}", category.as_str()))
            .reclaimable_bytes(bytes)
            .item_count(1)
            .reasoning("because");
        b = if inform {
            b.action(Action::InformOnly).instructions(Instructions {
                provider: "iCloud Drive".into(),
                summary: "s".into(),
                steps: Vec::new(),
            })
        } else {
            b.paths(vec![FindingPath {
                path: PathBuf::from("/Users/dana/Library/Caches/x"),
                size_bytes: bytes,
                last_used: None,
            }])
        };
        b.build().unwrap()
    }

    #[test]
    fn findings_are_grouped_by_risk_with_labels_totals_and_the_footer() {
        let report = SuggestReport::from_findings(vec![
            finding("user-cache.a", Category::UserCache, 16_800_000_000, false),
            finding("build-cache.b", Category::BuildCache, 121_400_000_000, false),
            finding("cloud-synced.c", Category::CloudSynced, 12_500_000_000, true),
        ]);

        let text = render(&report, false, Path::new("/Users/dana"), ColorPolicy::Never);

        assert!(text.starts_with("Potentially reclaimable space:  150.7 GB"), "{text}");
        assert!(text.contains("SAFE (green) — 138.2 GB"), "{text}");
        assert!(text.contains("   build-cache      121.4 GB   Title of build-cache (121.4 GB)"), "{text}");
        assert!(text.contains("INFO ONLY (red) — 12.5 GB"), "{text}");
        assert!(text.contains("→ Broza does not delete this. See: broza explain cloud-synced"), "{text}");
        assert!(text.ends_with("(moves to quarantine; space is freed after expiry or purge)"), "{text}");
    }

    #[test]
    fn explain_adds_the_reasoning_and_the_paths_shortened_to_home() {
        let report =
            SuggestReport::from_findings(vec![finding("user-cache.a", Category::UserCache, 4096, false)]);

        let text = render(&report, true, Path::new("/Users/dana"), ColorPolicy::Never);

        assert!(text.contains("user-cache.a: because"), "{text}");
        assert!(text.contains("~/Library/Caches/x"), "{text}");
    }

    #[test]
    fn an_empty_report_says_so() {
        let text =
            render(&SuggestReport::from_findings(Vec::new()), false, Path::new("/"), ColorPolicy::Never);

        assert_eq!(text, NOTHING_FOUND);
    }
}
