//! The human rendering of `broza suggest` (`docs/cli-spec.md` §3.3).
//!
//! Findings are grouped by risk, each group headed by its text label and its
//! total, then by category, biggest first. Every risk carries a word as well as
//! a colour (RNF-06). The headline counts only what Broza can act on; inform-only
//! findings are named on their own line with their size, so the number at the
//! top is never inflated by space Broza will not free. The footer names the two
//! commands that come next — the dry run first — for the safest non-empty group.

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
/// How many paths `--explain` prints per finding.
const EXPLAIN_PATHS: usize = 5;
/// What is printed when no detector found anything.
const NOTHING_FOUND: &str =
    "Nothing to suggest: no finding passed the --category, --risk and --min-size filters.";

/// Render `report` for a terminal.
pub fn render(report: &SuggestReport, explain: bool, home: &Path, policy: ColorPolicy) -> String {
    if report.findings.is_empty() {
        return NOTHING_FOUND.to_owned();
    }
    let mut text =
        format!("Potentially reclaimable space:  {}", format_bytes(report.total_reclaimable_bytes));
    if report.inform_only_bytes > 0 {
        let _ignored =
            write!(text, "\nReported, not reclaimable by Broza: {}", format_bytes(report.inform_only_bytes));
    }
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
        for (category, of_category) in by_category_biggest_first(&findings) {
            render_category(&mut text, category, &of_category, explain, home);
        }
    }
    if let Some(footer) = footer(report) {
        let _ignored = write!(text, "\n\n{footer}");
    }
    text
}

/// `SAFE (green)` and friends, with the label painted and the token in words.
fn heading(risk: Risk, policy: ColorPolicy) -> String {
    let token = match risk {
        Risk::Green => "green",
        Risk::Amber => "amber",
        Risk::Red => "red",
        _ => "unknown",
    };
    format!("{} ({token})", risk_chip(policy, risk))
}

/// The findings of one risk group split by category, biggest category first.
fn by_category_biggest_first<'a>(findings: &[&'a Finding]) -> Vec<(Category, Vec<&'a Finding>)> {
    let mut groups: Vec<(Category, Vec<&Finding>)> = Category::all()
        .into_iter()
        .map(|category| (category, findings.iter().copied().filter(|f| f.category() == category).collect()))
        .filter(|(_, of_category): &(Category, Vec<&Finding>)| !of_category.is_empty())
        .collect();
    groups.sort_by_key(|(_, of_category)| std::cmp::Reverse(actionable_total(of_category)));
    groups
}

/// The bytes Broza can act on among `findings`.
fn actionable_total(findings: &[&Finding]) -> u64 {
    findings.iter().filter(|f| f.is_actionable()).map(|f| f.reclaimable_bytes()).fold(0, u64::saturating_add)
}

/// One category line, with its actionable findings summarised, plus their details.
fn render_category(text: &mut String, category: Category, findings: &[&Finding], explain: bool, home: &Path) {
    let summary = findings
        .iter()
        .filter(|f| f.is_actionable())
        .map(|f| format!("{} ({})", f.title(), format_bytes(f.reclaimable_bytes())))
        .collect::<Vec<_>>()
        .join(", ");
    let _ignored = write!(
        text,
        "\n   {:<CATEGORY_WIDTH$}{:>SIZE_WIDTH$}   {summary}",
        category.as_str(),
        format_bytes(actionable_total(findings))
    );
    for finding in findings {
        if !finding.is_actionable() {
            let _ignored = write!(
                text,
                "\n   {:<CATEGORY_WIDTH$}{:>SIZE_WIDTH$}   {}: {} — inform only, see: broza explain {}",
                "",
                "",
                finding.title(),
                format_bytes(finding.reclaimable_bytes()),
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
    let _ignored = write!(text, "\n{indent}{}", finding.id());
    if let Some(reasoning) = finding.reasoning() {
        let _ignored = write!(text, ": {reasoning}");
    }
    for path in finding.paths().iter().take(EXPLAIN_PATHS) {
        let _ignored = write!(
            text,
            "\n{indent}  {:>SIZE_WIDTH$}  {}",
            format_bytes(path.size_bytes),
            abbreviate(&path.path, Some(home))
        );
    }
    if finding.paths().len() > EXPLAIN_PATHS {
        let _ignored = write!(text, "\n{indent}  … and {} more", finding.paths().len() - EXPLAIN_PATHS);
    }
}

/// What to run next: the dry run, then the real thing, for the safest level
/// that has something to clean. Nothing when nothing is actionable.
fn footer(report: &SuggestReport) -> Option<String> {
    let level = [(Risk::Green, "green"), (Risk::Amber, "amber"), (Risk::Red, "red")]
        .into_iter()
        .find(|(risk, _)| report.findings.iter().any(|f| f.is_actionable() && f.risk() == *risk))
        .map(|(_, token)| token)?;
    Some(format!(
        "Next step:\n  broza clean --risk {level}            (dry run, deletes nothing)\n  broza clean --risk {level} --apply    (moves to quarantine; space is freed after expiry or purge)"
    ))
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

        assert!(text.starts_with("Potentially reclaimable space:  138.2 GB"), "{text}");
        assert!(text.contains("Reported, not reclaimable by Broza: 12.5 GB"), "{text}");
        assert!(text.contains("SAFE (green) — 138.2 GB"), "{text}");
        assert!(text.contains("   build-cache      121.4 GB   Title of build-cache (121.4 GB)"), "{text}");
        assert!(text.contains("INFO ONLY (red) — 0 B"), "{text}");
        assert!(
            text.contains("Title of cloud-synced: 12.5 GB — inform only, see: broza explain cloud-synced"),
            "{text}"
        );
        assert!(text.contains("broza clean --risk green            (dry run"), "{text}");
        assert!(text.ends_with("(moves to quarantine; space is freed after expiry or purge)"), "{text}");
    }

    #[test]
    fn categories_within_a_group_are_listed_biggest_first() {
        let report = SuggestReport::from_findings(vec![
            finding("user-cache.a", Category::UserCache, 16_800_000_000, false),
            finding("build-cache.b", Category::BuildCache, 121_400_000_000, false),
        ]);

        let text = render(&report, false, Path::new("/Users/dana"), ColorPolicy::Never);

        assert!(text.find("build-cache").unwrap() < text.find("user-cache").unwrap(), "{text}");
    }

    #[test]
    fn the_footer_names_the_safest_level_with_something_to_clean_or_is_absent() {
        let amber_only = SuggestReport::from_findings(vec![
            finding("trash.a", Category::Trash, 1_000_000, false),
            finding("cloud-synced.c", Category::CloudSynced, 12_500_000_000, true),
        ]);
        let inform_only =
            SuggestReport::from_findings(vec![finding("cloud-synced.c", Category::CloudSynced, 5, true)]);

        let amber = render(&amber_only, false, Path::new("/"), ColorPolicy::Never);
        let nothing = render(&inform_only, false, Path::new("/"), ColorPolicy::Never);

        assert!(amber.contains("broza clean --risk amber --apply"), "{amber}");
        assert!(!nothing.contains("Next step"), "{nothing}");
        assert!(nothing.starts_with("Potentially reclaimable space:  0 B"), "{nothing}");
    }

    #[test]
    fn explain_adds_the_id_the_reasoning_and_the_paths_shortened_to_home() {
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
