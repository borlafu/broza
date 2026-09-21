//! The human renderings of `broza restore` (`docs/cli-spec.md` §3.5).

use std::fmt::Write as _;
use std::path::Path;

use broza::model::{ItemStatus, QuarantineEntry, RestoreReport, Warning};

use crate::output::format_bytes;
use crate::output::human::scan::folders::abbreviate;

/// Width of the status column.
const STATUS_WIDTH: usize = 10;
/// Width of the size column.
const SIZE_WIDTH: usize = 9;
/// Width of the item-id column in `--list`.
const ID_WIDTH: usize = 30;

/// `restore --list`: every item still in the store, by session.
pub fn render_list(report: &RestoreReport, errors: &[Warning], home: &Path) -> String {
    if report.sessions.iter().all(|session| session.items.is_empty()) && errors.is_empty() {
        return "Quarantine is empty.".to_owned();
    }
    let mut text = format!("{:<ID_WIDTH$}{:>SIZE_WIDTH$}  Original path", "Item", "Size");
    for error in errors {
        let _ignored = write!(text, "\n{}: {}", error.code, error.message);
    }
    for session in &report.sessions {
        for item in &session.items {
            let _ignored = write!(
                text,
                "\n{:<ID_WIDTH$}{:>SIZE_WIDTH$}  {}",
                item.id.to_string(),
                format_bytes(item.size_bytes),
                abbreviate(&item.original_path, Some(home))
            );
        }
    }
    let _ignored = write!(
        text,
        "\n\nRestore with: broza restore <ID>  ·  a whole session: broza restore --session <cln_…>"
    );
    text
}

/// `restore`: what went back, what did not and why.
pub fn render_restore(report: &RestoreReport, errors: &[Warning], home: &Path) -> String {
    if report.sessions.is_empty() && errors.is_empty() {
        return "Quarantine is empty.".to_owned();
    }
    let restored = report
        .sessions
        .iter()
        .flat_map(|session| &session.items)
        .filter(|item| item.status == ItemStatus::Restored)
        .count();
    let mut text = format!("Restored {restored} item(s), {} in all.", format_bytes(report.restored_bytes));
    for session in &report.sessions {
        for item in session.items.iter().filter(|item| item.status != ItemStatus::Restored) {
            render_item(&mut text, item, home);
        }
    }
    for error in errors {
        let _ignored = write!(text, "\n   {}: {}", error.code, error.message);
    }
    text
}

fn render_item(text: &mut String, item: &QuarantineEntry, home: &Path) {
    let status = match item.status {
        ItemStatus::Skipped => "skipped",
        ItemStatus::Failed => "failed",
        _ => "other",
    };
    let _ignored = write!(
        text,
        "\n   {status:<STATUS_WIDTH$}{:>SIZE_WIDTH$}  {}",
        format_bytes(item.size_bytes),
        abbreviate(&item.original_path, Some(home))
    );
    if let Some(error) = &item.error {
        let hint = if error.to_string() == "collision" {
            "; something is at the original path, use --to"
        } else {
            ""
        };
        let _ignored = write!(text, "  ({error}{hint})");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn report(status: &str, error: Option<&str>) -> RestoreReport {
        let mut item = serde_json::json!({
            "id": "cln_20260917103608_a1b2/0001",
            "original_path": "/Users/dana/Library/Caches/x",
            "size_bytes": 4096,
            "status": status
        });
        if let Some(code) = error {
            item["error"] = serde_json::Value::String(code.to_owned());
        }
        serde_json::from_value(serde_json::json!({
            "operation": "restore",
            "restored_bytes": if status == "restored" { 4096 } else { 0 },
            "sessions": [{"id": "cln_20260917103608_a1b2", "status": status, "items": [item]}]
        }))
        .unwrap()
    }

    #[test]
    fn a_restore_counts_what_went_back_and_explains_a_collision() {
        let fine = render_restore(&report("restored", None), &[], Path::new("/Users/dana"));
        let stuck = render_restore(&report("skipped", Some("collision")), &[], Path::new("/Users/dana"));

        assert_eq!(fine, "Restored 1 item(s), 4.1 KB in all.");
        assert!(
            stuck.contains("skipped      4.1 KB  ~/Library/Caches/x  (collision; something is at the original path, use --to)"),
            "{stuck}"
        );
    }

    #[test]
    fn the_list_names_each_item_with_its_id_and_the_next_command() {
        let text = render_list(&report("planned", None), &[], Path::new("/Users/dana"));

        assert!(text.contains("cln_20260917103608_a1b2/0001     4.1 KB  ~/Library/Caches/x"), "{text}");
        assert!(text.ends_with("broza restore --session <cln_…>"), "{text}");
    }
}
