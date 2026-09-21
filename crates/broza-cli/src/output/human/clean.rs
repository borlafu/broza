//! The human rendering of `broza clean` (`docs/cli-spec.md` §3.4).
//!
//! A dry run says so in its first line and never pretends to have moved
//! anything; an applied run names the session, what moved, what did not and
//! why, and how to undo it. Pending bytes (quarantined) and freed bytes
//! (expired sessions) are never added together (`AGENTS.md` §2.7).

use std::fmt::Write as _;
use std::path::Path;

use broza::model::{CleanItem, CleanPlan, ItemStatus, SessionId, Warning};

use crate::output::human::scan::folders::abbreviate;
use crate::output::{ColorPolicy, format_bytes};

/// Width of the status column.
const STATUS_WIDTH: usize = 12;
/// Width of the size column.
const SIZE_WIDTH: usize = 9;
/// How many planned items a dry run lists before folding the rest.
const DRY_RUN_ITEMS: usize = 40;

/// Render `plan` for a terminal.
pub fn render(
    plan: &CleanPlan,
    due: &[(SessionId, u64)],
    errors: &[Warning],
    home: &Path,
    _policy: ColorPolicy,
) -> String {
    if plan.is_dry_run() { render_dry_run(plan, due, home) } else { render_applied(plan, errors, home) }
}

fn render_dry_run(plan: &CleanPlan, due: &[(SessionId, u64)], home: &Path) -> String {
    let mut text = String::from("Dry run: nothing was changed.");
    if plan.items().is_empty() {
        let _ignored =
            write!(text, "\nNothing matches the selection; `broza suggest` lists what could be cleaned.");
        return text;
    }
    let _ignored = write!(
        text,
        "\n\nWould move {} item(s), {} in all, to quarantine:",
        plan.items().len(),
        format_bytes(plan.planned_bytes())
    );
    for item in plan.items().iter().take(DRY_RUN_ITEMS) {
        render_item(&mut text, item, home);
    }
    if plan.items().len() > DRY_RUN_ITEMS {
        let _ignored = write!(text, "\n   … and {} more", plan.items().len() - DRY_RUN_ITEMS);
    }
    if !due.is_empty() {
        let bytes = due.iter().map(|(_, bytes)| *bytes).fold(0, u64::saturating_add);
        let _ignored = write!(
            text,
            "\n\n{} quarantine session(s) past their retention hold {}; --apply expires them and frees that space.",
            due.len(),
            format_bytes(bytes)
        );
    }
    let _ignored = write!(
        text,
        "\n\nNext step:\n  broza clean {} --apply    (moves to quarantine; `broza restore` brings items back until they expire)",
        selection_hint(plan)
    );
    text
}

fn render_applied(plan: &CleanPlan, errors: &[Warning], home: &Path) -> String {
    let moved = plan.items().iter().filter(|item| item.status == ItemStatus::Quarantined).count();
    let mut text = format!(
        "Session {}: {moved} item(s) moved to quarantine, {} pending (freed after expiry or purge).",
        plan.session_id(),
        format_bytes(plan.quarantined_bytes())
    );
    for item in plan.items().iter().filter(|item| item.status.is_unsuccessful()) {
        render_item(&mut text, item, home);
    }
    if !plan.expired_sessions().is_empty() {
        let _ignored = write!(
            text,
            "\nExpired {} older session(s): {} freed.",
            plan.expired_sessions().len(),
            format_bytes(plan.reclaimed_bytes())
        );
    }
    if !errors.is_empty() {
        let _ignored =
            write!(text, "\n{} item(s) or session(s) could not be processed (exit 5).", errors.len());
    }
    if let Some(path) = plan.quarantine_path() {
        let _ignored = write!(text, "\nQuarantine: {}", abbreviate(path, Some(home)));
    }
    let _ignored = write!(text, "\nUndo: broza restore --session {}", plan.session_id());
    text
}

/// One item line: status, size, path and, when it was not moved, the reason.
fn render_item(text: &mut String, item: &CleanItem, home: &Path) {
    let status = status_word(&item.status);
    let _ignored = write!(
        text,
        "\n   {status:<STATUS_WIDTH$}{:>SIZE_WIDTH$}  {}",
        format_bytes(item.size_bytes),
        abbreviate(&item.path, Some(home))
    );
    if let Some(error) = &item.error {
        let _ignored = write!(text, "  ({error}{})", hint_for(error.to_string().as_str()));
    }
}

fn status_word(status: &ItemStatus) -> &'static str {
    match status {
        ItemStatus::Planned => "planned",
        ItemStatus::Quarantined => "quarantined",
        ItemStatus::Purged => "purged",
        ItemStatus::Restored => "restored",
        ItemStatus::Skipped => "skipped",
        ItemStatus::Failed => "failed",
        ItemStatus::Unknown(_) | _ => "unknown",
    }
}

/// What to do about an item that was not moved, when there is something to do.
fn hint_for(code: &str) -> &'static str {
    match code {
        "cross_volume" => "; set quarantine-path on that volume or use --purge",
        "max_size_exceeded" => "; raise --max-size or clean it on its own",
        "permission_denied" => "; grant Full Disk Access or remove it yourself",
        _ => "",
    }
}

/// The `--category`/`--risk` spelling that reproduces this plan's selection.
fn selection_hint(plan: &CleanPlan) -> String {
    let mut categories: Vec<&str> = plan.items().iter().map(|item| item.finding_id.category_part()).collect();
    categories.sort_unstable();
    categories.dedup();
    categories.iter().map(|category| format!("--category {category}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::PathBuf;

    use broza::model::{Action, ItemErrorCode};

    use super::*;

    fn item(path: &str, bytes: u64, status: ItemStatus, error: Option<ItemErrorCode>) -> CleanItem {
        CleanItem {
            path: PathBuf::from(path),
            finding_id: "build-cache.pycache".parse().unwrap(),
            size_bytes: bytes,
            status,
            action: Action::Quarantine,
            error,
        }
    }

    fn session() -> SessionId {
        "cln_20260921103608_a1b2".parse().unwrap()
    }

    #[test]
    fn a_dry_run_says_so_lists_the_items_and_names_the_next_command() {
        let plan = CleanPlan::dry_run(
            session(),
            vec![item("/Users/dana/code/x/__pycache__", 4096, ItemStatus::Planned, None)],
        )
        .unwrap();

        let text = render(&plan, &[(session(), 500)], &[], Path::new("/Users/dana"), ColorPolicy::Never);

        assert!(text.starts_with("Dry run: nothing was changed."), "{text}");
        assert!(text.contains("Would move 1 item(s), 4.1 KB in all"), "{text}");
        assert!(text.contains("planned        4.1 KB  ~/code/x/__pycache__"), "{text}");
        assert!(text.contains("1 quarantine session(s) past their retention hold 500 B"), "{text}");
        assert!(text.contains("broza clean --category build-cache --apply"), "{text}");
    }

    #[test]
    fn an_applied_run_names_the_session_the_skips_and_the_undo() {
        let plan = CleanPlan::dry_run(
            session(),
            vec![
                item("/Users/dana/a", 10, ItemStatus::Planned, None),
                item("/Volumes/Ext/b", 5, ItemStatus::Planned, None),
            ],
        )
        .unwrap()
        .into_applied(Some(PathBuf::from(
            "/Users/dana/.local/share/broza/quarantine/cln_20260921103608_a1b2",
        )))
        .unwrap()
        .with_item_status(0, ItemStatus::Quarantined, None)
        .unwrap()
        .with_item_status(1, ItemStatus::Skipped, Some(ItemErrorCode::CrossVolume))
        .unwrap()
        .with_bytes(10, 0)
        .unwrap();

        let text = render(&plan, &[], &[], Path::new("/Users/dana"), ColorPolicy::Never);

        assert!(
            text.starts_with("Session cln_20260921103608_a1b2: 1 item(s) moved to quarantine, 10 B pending"),
            "{text}"
        );
        assert!(
            text.contains("skipped           5 B  /Volumes/Ext/b  (cross_volume; set quarantine-path"),
            "{text}"
        );
        assert!(
            text.contains("Quarantine: ~/.local/share/broza/quarantine/cln_20260921103608_a1b2"),
            "{text}"
        );
        assert!(text.ends_with("Undo: broza restore --session cln_20260921103608_a1b2"), "{text}");
    }
}
