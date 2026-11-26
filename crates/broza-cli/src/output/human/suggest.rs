//! The human rendering of `broza suggest` (`docs/cli-spec.md` §3.3).
//!
//! Actionable findings are grouped by risk, each group headed by its text label
//! and its total, then by category, biggest first. Every risk carries a word as
//! well as a colour (RNF-06). The headline counts only what Broza can act on.
//! Inform-only findings — space the user can reclaim another way, such as
//! Docker's disk image — get a group of their own after the risk groups, with
//! their size and the steps that free it, so the number at the top is never
//! inflated by space Broza will not free and a `0 B` row never hides gigabytes.
//! The footer names the two commands that come next — the dry run first — for
//! the safest non-empty group.

use std::fmt::Write as _;
use std::path::Path;

use broza::model::{Category, Finding, Risk, SuggestReport};

use crate::output::human::risk_chip;
use crate::output::human::scan::folders::abbreviate;
use crate::output::wrap::wrap;
use crate::output::{ColorPolicy, format_bytes};

/// Width of the category column.
const CATEGORY_WIDTH: usize = 16;
/// Width of the size column.
const SIZE_WIDTH: usize = 9;
/// How many paths `--explain` prints per finding.
const EXPLAIN_PATHS: usize = 5;
/// Label of the headline line and of the group for inform-only findings.
const ANOTHER_WAY: &str = "Reclaimable another way";
/// Indent of everything printed under a category row's description column.
const DETAIL_INDENT: &str = "                               ";
/// Hanging indent of a numbered step, after the number.
const STEP_INDENT: &str = "   ";
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
        let _ignored = write!(
            text,
            "\n{:<32}{}   (Broza only reports these; steps below)",
            format!("{ANOTHER_WAY}:"),
            format_bytes(report.inform_only_bytes)
        );
    }
    for (risk, total) in [
        (Risk::Green, report.by_risk.green),
        (Risk::Amber, report.by_risk.amber),
        (Risk::Red, report.by_risk.red),
    ] {
        let findings: Vec<&Finding> =
            report.findings.iter().filter(|f| f.risk() == risk && f.is_actionable()).collect();
        if findings.is_empty() {
            continue;
        }
        let _ignored = write!(text, "\n\n{} — {}", heading(risk, policy), format_bytes(total));
        for (category, of_category) in by_category_biggest_first(&findings) {
            render_category(&mut text, category, &of_category, explain, home);
        }
    }
    render_another_way(&mut text, report, explain, home);
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
    let summary = findings.iter().map(|f| summary_of(f)).collect::<Vec<_>>().join(", ");
    let _ignored = write!(
        text,
        "\n   {:<CATEGORY_WIDTH$}{:>SIZE_WIDTH$}   {summary}",
        category.as_str(),
        format_bytes(actionable_total(findings))
    );
    if explain {
        for finding in findings {
            render_reasoning(text, finding, home);
        }
    }
}

/// The inform-only findings, biggest first, each with its size and the steps
/// that free the space outside Broza. Nothing when there are none.
fn render_another_way(text: &mut String, report: &SuggestReport, explain: bool, home: &Path) {
    let mut informed: Vec<&Finding> = report.findings.iter().filter(|f| !f.is_actionable()).collect();
    if informed.is_empty() {
        return;
    }
    informed.sort_by_key(|f| std::cmp::Reverse(f.reclaimable_bytes()));
    let _ignored = write!(
        text,
        "\n\n{} — {}   (not by Broza)",
        ANOTHER_WAY.to_uppercase(),
        format_bytes(report.inform_only_bytes)
    );
    for finding in informed {
        let _ignored = write!(
            text,
            "\n   {:<CATEGORY_WIDTH$}{:>SIZE_WIDTH$}   {}",
            finding.category().as_str(),
            format_bytes(finding.reclaimable_bytes()),
            finding.title()
        );
        render_steps(text, finding);
        if explain {
            render_reasoning(text, finding, home);
        }
    }
}

/// The provider's summary and numbered steps under an inform-only finding,
/// then where to read more.
fn render_steps(text: &mut String, finding: &Finding) {
    if let Some(instructions) = finding.instructions() {
        let _ignored = write!(text, "\n{}", wrap(&instructions.summary, DETAIL_INDENT));
        for (index, step) in instructions.steps.iter().enumerate() {
            let body = wrap(step, &format!("{DETAIL_INDENT}{STEP_INDENT}"));
            let numbered =
                format!("{DETAIL_INDENT}{:<width$}", format!("{}.", index + 1), width = STEP_INDENT.len());
            let _ignored = write!(text, "\n{numbered}{}", &body[numbered.len()..]);
        }
    }
    let _ignored = write!(text, "\n{DETAIL_INDENT}More: broza explain {}", finding.category().as_str());
}

/// `Title (size)`, or `N Title (reason)` for a finding whose size macOS does
/// not report (the snapshots: `4 Time Machine local snapshots (size not reported by macOS)`).
fn summary_of(finding: &Finding) -> String {
    match (finding.size_is_unknown(), finding.item_count(), finding.reasoning()) {
        (true, Some(count), Some(reason)) => format!("{count} {} ({reason})", finding.title()),
        _ => format!("{} ({})", finding.title(), format_bytes(finding.reclaimable_bytes())),
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
                summary: "Let the provider keep the file online only.".into(),
                steps: vec!["Open Finder.".into(), "Choose Remove Download.".into()],
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
        assert!(
            text.contains(
                "Reclaimable another way:        12.5 GB   (Broza only reports these; steps below)"
            ),
            "{text}"
        );
        assert!(text.contains("SAFE (green) — 138.2 GB"), "{text}");
        assert!(text.contains("   build-cache      121.4 GB   Title of build-cache (121.4 GB)"), "{text}");
        assert!(!text.contains("INFO ONLY"), "a risk group with nothing actionable is not printed: {text}");
        assert!(!text.contains(" 0 B"), "no zero row hides the inform-only size: {text}");
        assert!(text.contains("RECLAIMABLE ANOTHER WAY — 12.5 GB   (not by Broza)"), "{text}");
        assert!(text.contains("   cloud-synced      12.5 GB   Title of cloud-synced"), "{text}");
        assert!(text.contains("broza clean --risk green            (dry run"), "{text}");
        assert!(text.ends_with("(moves to quarantine; space is freed after expiry or purge)"), "{text}");
    }

    #[test]
    fn an_inform_only_finding_shows_its_size_its_steps_and_where_to_read_more() {
        let report = SuggestReport::from_findings(vec![
            finding("user-cache.a", Category::UserCache, 16_800_000_000, false),
            finding("cloud-synced.c", Category::CloudSynced, 12_500_000_000, true),
        ]);

        let text = render(&report, false, Path::new("/Users/dana"), ColorPolicy::Never);

        let group = &text[text.find("RECLAIMABLE ANOTHER WAY").unwrap()..text.find("Next step").unwrap()];
        assert_eq!(
            group.trim_end(),
            "RECLAIMABLE ANOTHER WAY — 12.5 GB   (not by Broza)
   cloud-synced      12.5 GB   Title of cloud-synced
                               Let the provider keep the file online only.
                               1. Open Finder.
                               2. Choose Remove Download.
                               More: broza explain cloud-synced"
        );
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
