//! The human rendering of `broza clean` (`docs/cli-spec.md` §3.4).
//!
//! A dry run says so in its first line and never pretends: it names the action
//! the plan really carries (a move to quarantine, or with `--purge` a permanent
//! deletion) and repeats the exact flags that produced it. An applied run
//! names the session, what moved, what did not and why, and how to undo it.
//! Pending bytes (quarantined) and freed bytes (expired sessions) are never
//! added together (`AGENTS.md` §2.7).

use std::fmt::Write as _;
use std::path::Path;

use broza::model::{Action, CleanItem, CleanPlan, ItemStatus, SessionId, Warning};

use crate::output::format_bytes;
use crate::output::human::scan::folders::abbreviate;

/// Width of the status column.
const STATUS_WIDTH: usize = 12;
/// Width of the size column.
const SIZE_WIDTH: usize = 9;
/// How many planned items a dry run lists before folding the rest.
const DRY_RUN_ITEMS: usize = 40;

/// Render `plan` for a terminal. `rerun` is the `broza clean …` line that
/// reproduces the selection, built from the flags by the command.
pub fn render(
    plan: &CleanPlan,
    due: &[(SessionId, u64)],
    errors: &[Warning],
    home: &Path,
    rerun: &str,
) -> String {
    if plan.is_dry_run() {
        render_dry_run(plan, due, home, rerun)
    } else if plan.quarantine_path().is_none() {
        // No session was created: every planned path was gone, or there was
        // nothing to plan. There is nothing to undo either.
        render_nothing_moved(plan, errors, home)
    } else {
        render_applied(plan, errors, home)
    }
}

/// `true` when the plan would delete for good rather than move to quarantine.
fn is_purge(plan: &CleanPlan) -> bool {
    plan.items().iter().any(|item| item.action == Action::Purge)
}

fn render_dry_run(plan: &CleanPlan, due: &[(SessionId, u64)], home: &Path, rerun: &str) -> String {
    let mut text = String::from("Dry run: nothing was changed.");
    if plan.items().is_empty() {
        let _ignored =
            write!(text, "\nNothing matches the selection; `broza suggest` lists what could be cleaned.");
        return text;
    }
    let action =
        if is_purge(plan) { "delete permanently (no quarantine, no restore)" } else { "move to quarantine" };
    let _ignored = write!(
        text,
        "\n\nWould {action} {} item(s), {} in all:",
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
    if is_purge(plan) {
        let _ignored = write!(
            text,
            "\n\nNote: irreversible deletion lands in milestone M4; `{rerun} --apply` is refused today.\nQuarantine the items instead ({} --apply) and `broza quarantine purge` the session afterwards.",
            rerun.strip_suffix(" --purge").unwrap_or(rerun)
        );
        return text;
    }
    let _ignored = write!(
        text,
        "\n\nNext step:\n  {rerun} --apply    (moves to quarantine; `broza restore` brings items back until they expire)"
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
    render_expiry(&mut text, plan);
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

/// `--apply` with nothing to move: no session was created, so nothing to undo.
fn render_nothing_moved(plan: &CleanPlan, errors: &[Warning], home: &Path) -> String {
    let mut text = String::from("Nothing to move: no item of the selection is left to clean.");
    for item in plan.items().iter().filter(|item| item.status.is_unsuccessful()) {
        render_item(&mut text, item, home);
    }
    render_expiry(&mut text, plan);
    if !errors.is_empty() {
        let _ignored =
            write!(text, "\n{} item(s) or session(s) could not be processed (exit 5).", errors.len());
    }
    text
}

/// The expiry line, from the expired sessions' own figures.
fn render_expiry(text: &mut String, plan: &CleanPlan) {
    if plan.expired_sessions().is_empty() {
        return;
    }
    let freed = plan.expired_sessions().iter().map(|s| s.freed_bytes).fold(0, u64::saturating_add);
    let _ignored = write!(
        text,
        "\nExpired {} older session(s): {} freed.",
        plan.expired_sessions().len(),
        format_bytes(freed)
    );
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
        _ => "unknown",
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::PathBuf;

    use broza::model::ItemErrorCode;

    use super::*;

    const RERUN: &str = "broza clean --risk green";

    fn item(path: &str, bytes: u64, action: Action) -> CleanItem {
        CleanItem {
            path: PathBuf::from(path),
            finding_id: "build-cache.pycache".parse().unwrap(),
            size_bytes: bytes,
            status: ItemStatus::Planned,
            action,
            error: None,
        }
    }

    fn session() -> SessionId {
        "cln_20260921103608_a1b2".parse().unwrap()
    }

    #[test]
    fn a_dry_run_says_so_lists_the_items_and_repeats_the_flags_it_was_given() {
        let plan = CleanPlan::dry_run(
            session(),
            vec![item("/Users/dana/code/x/__pycache__", 4096, Action::Quarantine)],
        )
        .unwrap();

        let text = render(&plan, &[(session(), 500)], &[], Path::new("/Users/dana"), RERUN);

        assert!(text.starts_with("Dry run: nothing was changed."), "{text}");
        assert!(text.contains("Would move to quarantine 1 item(s), 4.1 KB in all"), "{text}");
        assert!(text.contains("planned        4.1 KB  ~/code/x/__pycache__"), "{text}");
        assert!(text.contains("1 quarantine session(s) past their retention hold 500 B"), "{text}");
        assert!(text.contains("  broza clean --risk green --apply    (moves to quarantine;"), "{text}");
    }

    #[test]
    fn a_purge_dry_run_never_promises_a_restore() {
        let plan =
            CleanPlan::dry_run(session(), vec![item("/Users/dana/.Trash/x", 4096, Action::Purge)]).unwrap();

        let text = render(&plan, &[], &[], Path::new("/Users/dana"), "broza clean --category trash --purge");

        assert!(text.contains("Would delete permanently (no quarantine, no restore) 1 item(s)"), "{text}");
        assert!(text.contains("irreversible deletion lands in milestone M4"), "{text}");
        assert!(text.contains("(broza clean --category trash --apply)"), "{text}");
        assert!(!text.contains("restore` brings"), "{text}");
    }

    #[test]
    fn an_applied_run_names_the_session_the_skips_and_the_undo() {
        let plan = CleanPlan::dry_run(
            session(),
            vec![
                item("/Users/dana/a", 10, Action::Quarantine),
                item("/Volumes/Ext/b", 5, Action::Quarantine),
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

        let text = render(&plan, &[], &[], Path::new("/Users/dana"), RERUN);

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

    #[test]
    fn an_applied_run_whose_every_path_vanished_lists_the_skips_and_offers_no_undo() {
        let plan = CleanPlan::dry_run(
            session(),
            vec![item("/Users/dana/Library/Caches/gone", 0, Action::Quarantine)],
        )
        .unwrap()
        .into_applied(None)
        .unwrap()
        .with_item_status(0, ItemStatus::Skipped, Some(ItemErrorCode::NotFound))
        .unwrap();

        let text = render(&plan, &[], &[], Path::new("/Users/dana"), RERUN);

        assert!(text.starts_with("Nothing to move:"), "{text}");
        assert!(text.contains("skipped           0 B  ~/Library/Caches/gone  (not_found)"), "{text}");
        assert!(!text.contains("Session ") && !text.contains("Undo"), "{text}");
    }

    #[test]
    fn an_applied_run_with_nothing_to_move_reports_only_the_expiry_and_no_undo() {
        let expired = broza::model::ExpiredSession { id: session(), freed_bytes: 700 };
        let plan = CleanPlan::dry_run(session(), Vec::new())
            .unwrap()
            .into_applied(None)
            .unwrap()
            .with_expired_sessions(vec![expired])
            .unwrap()
            .with_bytes(0, 700)
            .unwrap();

        let text = render(&plan, &[], &[], Path::new("/Users/dana"), RERUN);

        assert!(text.starts_with("Nothing to move:"), "{text}");
        assert!(text.contains("Expired 1 older session(s): 700 B freed."), "{text}");
        assert!(!text.contains("Undo"), "{text}");
    }
}
