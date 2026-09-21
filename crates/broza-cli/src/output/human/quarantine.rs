//! The human renderings of `broza quarantine` (`docs/cli-spec.md` §3.8).

use std::fmt::Write as _;

use broza::model::{ItemStatus, OperationKind, QuarantineList, QuarantineSession, ReclaimReport, Warning};
use jiff::Timestamp;

use crate::output::format_bytes;

/// `strftime` layout of the two date columns.
const DATE_LAYOUT: &str = "%Y-%m-%d %H:%M";
/// Width of the session column.
const SESSION_WIDTH: usize = 27;
/// Width of a date column.
const DATE_WIDTH: usize = 21;
/// Width of the size column.
const SIZE_WIDTH: usize = 10;
/// Width of the item-count column.
const ITEMS_WIDTH: usize = 7;

/// `quarantine list`: one line per session, then what is pending.
pub fn render_list(list: &QuarantineList, errors: &[Warning]) -> String {
    if list.sessions.is_empty() && errors.is_empty() {
        return "Quarantine is empty.".to_owned();
    }
    let mut text = format!(
        "{:<SESSION_WIDTH$}{:<DATE_WIDTH$}{:<DATE_WIDTH$}{:<SIZE_WIDTH$} {:<ITEMS_WIDTH$}State",
        "Session", "Created", "Expires", "Size", "Items"
    );
    for session in &list.sessions {
        render_session(&mut text, session);
    }
    for error in errors {
        let _ignored = write!(text, "\n{}: {}", error.code, error.message);
    }
    let _ignored = write!(text, "\n\nPending in quarantine:  {}", format_bytes(list.total_bytes));
    if list.expired_bytes > 0 {
        let _ignored =
            write!(text, "   ({} past TTL — run: broza quarantine expire)", format_bytes(list.expired_bytes));
    }
    text
}

fn render_session(text: &mut String, session: &QuarantineSession) {
    let _ignored = write!(
        text,
        "\n{:<SESSION_WIDTH$}{:<DATE_WIDTH$}{:<DATE_WIDTH$}{:<SIZE_WIDTH$} {:<ITEMS_WIDTH$}{}",
        session.id.to_string(),
        date(session.created_at),
        date(session.expires_at),
        format_bytes(session.total_bytes),
        session.item_count,
        session.state.as_str()
    );
}

/// `YYYY-MM-DD HH:MM` in UTC, the same clock the JSON uses.
fn date(at: Timestamp) -> String {
    at.strftime(DATE_LAYOUT).to_string()
}

/// `quarantine expire|purge`: what was removed and what was not (`docs/cli-spec.md` §3.8).
pub fn render_reclaim(report: &ReclaimReport, errors: &[Warning]) -> String {
    let verb = match report.operation {
        OperationKind::Expire => "expire",
        _ => "purge",
    };
    let mut text = if report.sessions.is_empty() {
        format!("Nothing to {verb}.")
    } else {
        format!("Freed {}.", format_bytes(report.reclaimed_bytes))
    };
    for session in &report.sessions {
        let _ignored = write!(
            text,
            "\n   {:<12}{:>SIZE_WIDTH$}  {}  ({} item(s))",
            status_word(&session.status),
            format_bytes(session.total_bytes),
            session.id,
            session.item_count
        );
    }
    for error in errors {
        let _ignored = write!(text, "\n   {}: {}", error.code, error.message);
    }
    text
}

fn status_word(status: &ItemStatus) -> &'static str {
    match status {
        ItemStatus::Purged => "removed",
        ItemStatus::Skipped => "skipped",
        ItemStatus::Failed => "failed",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_list_has_the_columns_of_the_specification_and_the_pending_line() {
        let list: QuarantineList = serde_json::from_value(serde_json::json!({
            "quarantine_path": "/q",
            "total_bytes": 150_600_000_000_u64,
            "expired_bytes": 12_400_000_000_u64,
            "sessions": [{
                "id": "cln_20260917103608_a1b2",
                "created_at": "2026-09-17T10:36:08Z",
                "expires_at": "2026-10-17T10:36:08Z",
                "total_bytes": 138_200_000_000_u64,
                "item_count": 1284,
                "state": "complete"
            }]
        }))
        .unwrap();

        let text = render_list(&list, &[]);

        assert!(text.starts_with("Session                    Created              Expires"), "{text}");
        assert!(text.contains("cln_20260917103608_a1b2    2026-09-17 10:36     2026-10-17 10:36     138.2 GB   1284   complete"), "{text}");
        assert!(
            text.ends_with(
                "Pending in quarantine:  150.6 GB   (12.4 GB past TTL — run: broza quarantine expire)"
            ),
            "{text}"
        );
    }

    #[test]
    fn an_empty_reclaim_says_nothing_to_do_and_a_full_one_counts_sessions() {
        let empty =
            ReclaimReport { operation: OperationKind::Expire, reclaimed_bytes: 0, sessions: Vec::new() };
        let full: ReclaimReport = serde_json::from_value(serde_json::json!({
            "operation": "purge",
            "reclaimed_bytes": 138_200_000_000_u64,
            "sessions": [{"id": "cln_20260917103608_a1b2", "total_bytes": 138_200_000_000_u64, "item_count": 1284, "status": "purged"}]
        }))
        .unwrap();

        assert_eq!(render_reclaim(&empty, &[]), "Nothing to expire.");
        let unreadable = Warning {
            code: "manifest_corrupt".into(),
            message: "quarantine session `/q/cln_x` cannot be read: bad json".into(),
            path: Some("/q/cln_x".into()),
        };
        let with_error = render_reclaim(&empty, &[unreadable]);
        assert!(with_error.starts_with("Nothing to expire.\n   manifest_corrupt: "), "{with_error}");
        let text = render_reclaim(&full, &[]);
        assert!(text.starts_with("Freed 138.2 GB."), "{text}");
        assert!(text.contains("removed       138.2 GB  cln_20260917103608_a1b2  (1284 item(s))"), "{text}");
    }
}
