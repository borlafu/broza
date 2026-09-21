//! The human rendering of `broza explain` (`docs/cli-spec.md` §3.2).
//!
//! One layout for all three kinds of target: a header line identifying what
//! was asked about, and then the same three sections — `What it is:`,
//! `What it is for:`, `Is it safe to touch?` — because the point of the
//! command is that a volume, a category and a path are all answered in the
//! same shape (design principle 3).
//!
//! The header differs only in what it can usefully say. A volume leads with
//! its name, its role qualified by the filesystem of its container
//! (`APFS role: Data`, `HFS+ role: User`) and its used size; a path adds one
//! line naming the volume it lives on; a category leads with its id, its risk
//! label and what Broza would do with it, in words.
//!
//! `--short` collapses the whole thing to one line of the form
//! `<target>  ·  <label>  ·  <summary>`: the shape `docs/cli-spec.md` §3.2
//! shows for a category, and the shape a volume and a path get too.
//!
//! Pure: a report and a colour policy in, a `String` out.

use std::fmt::Write as _;

use broza::detect::{ExplainKind, ExplainReport, category_summary};
use broza::model::{FsKind, Volume};

use crate::output::human::scan::role_label;
use crate::output::human::{action_label, risk_chip, role_style};
use crate::output::{ColorPolicy, format_bytes, paint, wrap};

/// Separator between the fields of a header or a `--short` line.
const HEADER_SEPARATOR: &str = "   ·   ";
/// Separator between the fields of a `--short` line, tighter than the header's.
const SHORT_SEPARATOR: &str = "  ·  ";
/// Indent of a wrapped paragraph.
const PARAGRAPH_INDENT: &str = "  ";
/// The three section titles, in order.
const SECTIONS: [&str; 3] = ["What it is:", "What it is for:", "Is it safe to touch?"];
/// Said when a report somehow carries neither a volume nor a category.
const UNKNOWN_TARGET: &str = "Unknown target";

/// Render `report` in full.
pub fn render(report: &ExplainReport, policy: ColorPolicy) -> String {
    let mut text = header(report, policy);
    let paragraphs =
        [&report.explanation.what_it_is, &report.explanation.what_it_is_for, &report.explanation.is_it_safe];
    for (title, body) in SECTIONS.iter().zip(paragraphs) {
        let _ignored = write!(text, "\n\n{title}\n{}", wrap(body, PARAGRAPH_INDENT));
    }
    text
}

/// Render `report` as the single line of `--short`.
pub fn render_short(report: &ExplainReport, policy: ColorPolicy) -> String {
    match (report.kind, report.category, report.volume.as_ref()) {
        (ExplainKind::Category, Some(category), _) => {
            let risk = report.risk.map_or_else(String::new, |risk| risk_chip(policy, risk));
            [category.as_str(), risk.as_str(), category_summary(category)].join(SHORT_SEPARATOR)
        }
        (_, _, Some(volume)) => {
            let target = target_name(report, volume);
            let role = paint(policy, role_style(volume.role), role_label(volume.role));
            [target, role, volume_summary(report, volume)].join(SHORT_SEPARATOR)
        }
        _ => UNKNOWN_TARGET.to_owned(),
    }
}

/// The first line: what was asked about, and its two headline facts.
fn header(report: &ExplainReport, policy: ColorPolicy) -> String {
    match (report.kind, report.category, report.volume.as_ref()) {
        (ExplainKind::Category, Some(category), _) => {
            let risk = report.risk.map_or_else(String::new, |risk| risk_chip(policy, risk));
            let action = report.action.map(action_label).unwrap_or_default();
            [category.as_str().to_owned(), risk, action.to_owned()].join(HEADER_SEPARATOR)
        }
        (_, _, Some(volume)) => volume_header(report, volume, policy),
        _ => UNKNOWN_TARGET.to_owned(),
    }
}

/// `Macintosh HD - Data   ·   APFS role: Data   ·   798.2 GB`, plus the path line.
fn volume_header(report: &ExplainReport, volume: &Volume, policy: ColorPolicy) -> String {
    let name = paint(policy, role_style(volume.role), &volume.name);
    let line = format!(
        "{name}{HEADER_SEPARATOR}{}role: {}{HEADER_SEPARATOR}{}",
        filesystem_prefix(report.filesystem.as_ref()),
        role_label(volume.role),
        format_bytes(volume.used_bytes)
    );
    match report.path.as_deref() {
        Some(path) => format!("{line}\n{} is on this volume ({}).", path.display(), volume.id),
        None => line,
    }
}

/// What qualifies the word `role:` in the header: the container's filesystem.
///
/// A volume on a mounted disk image is not an APFS volume, and saying so
/// anyway would be the kind of small confident falsehood this command exists
/// to avoid. When the caller could not say which filesystem it is, the header
/// says a plain `role:` rather than guessing.
fn filesystem_prefix(filesystem: Option<&FsKind>) -> String {
    match filesystem {
        Some(FsKind::Apfs) => "APFS ".to_owned(),
        Some(FsKind::HfsPlus) => "HFS+ ".to_owned(),
        Some(other) => format!("{other} "),
        None => String::new(),
    }
}

/// What the `--short` line names: the path when there is one, else the volume.
fn target_name(report: &ExplainReport, volume: &Volume) -> String {
    report.path.as_deref().map_or_else(|| volume.id.to_string(), |path| path.display().to_string())
}

/// The trailing sentence of a volume's `--short` line.
fn volume_summary(report: &ExplainReport, volume: &Volume) -> String {
    let where_it_is = match report.path {
        Some(_) => format!("on {} ({}), ", volume.name, volume.id),
        None => format!("{}, ", volume.name),
    };
    format!("{where_it_is}{}. {}", format_bytes(volume.used_bytes), volume.purpose)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::{Path, PathBuf};

    use broza::model::{Category, VolumeRole};

    use super::*;

    fn data_volume() -> Volume {
        Volume {
            id: "disk3s5".parse().unwrap_or_else(|e| panic!("{e}")),
            name: "Macintosh HD - Data".to_owned(),
            uuid: None,
            role: VolumeRole::Data,
            mount_point: Some(PathBuf::from("/System/Volumes/Data")),
            used_bytes: 798_210_000_000,
            writable_by_broza: true,
            purpose: "The writable volume that holds your home folder.".to_owned(),
        }
    }

    fn plain(report: &ExplainReport) -> String {
        render(report, ColorPolicy::Never)
    }

    #[test]
    fn a_volume_header_follows_the_specification() {
        let report = ExplainReport::for_volume(data_volume()).on_filesystem(FsKind::Apfs);

        let text = plain(&report);

        assert!(text.starts_with("Macintosh HD - Data   ·   APFS role: Data   ·   798.2 GB"), "{text}");
    }

    #[test]
    fn the_header_names_the_filesystem_it_was_told_about_and_never_guesses() {
        let volume = Volume { role: VolumeRole::User, name: "Kiro CLI".to_owned(), ..data_volume() };

        let hfs = plain(&ExplainReport::for_volume(volume.clone()).on_filesystem(FsKind::HfsPlus));
        let zfs =
            plain(&ExplainReport::for_volume(volume.clone()).on_filesystem(FsKind::Unknown("zfs".into())));
        let unknown = plain(&ExplainReport::for_volume(volume));

        assert!(hfs.starts_with("Kiro CLI   ·   HFS+ role: User"), "{hfs}");
        assert!(zfs.starts_with("Kiro CLI   ·   zfs role: User"), "{zfs}");
        assert!(unknown.starts_with("Kiro CLI   ·   role: User"), "{unknown}");
    }

    #[test]
    fn the_three_sections_are_always_present_and_in_order() {
        for report in [
            ExplainReport::for_volume(data_volume()),
            ExplainReport::for_category(Category::Snapshots),
            ExplainReport::for_path(Path::new("/Users"), data_volume()),
        ] {
            let text = plain(&report);
            let positions: Vec<Option<usize>> = SECTIONS.iter().map(|title| text.find(title)).collect();
            assert!(positions.iter().all(Option::is_some), "{text}");
            assert!(positions[0] < positions[1] && positions[1] < positions[2], "{text}");
        }
    }

    #[test]
    fn a_path_says_which_volume_it_is_on() {
        let text = plain(&ExplainReport::for_path(Path::new("/Users/dana"), data_volume()));

        assert!(text.contains("/Users/dana is on this volume (disk3s5)."), "{text}");
    }

    #[test]
    fn a_category_header_carries_its_risk_and_its_action_in_words() {
        let text = plain(&ExplainReport::for_category(Category::CloudSynced));

        assert!(text.starts_with("cloud-synced   ·   INFO ONLY"), "{text}");
        assert!(text.contains("never deletes this"), "{text}");
    }

    #[test]
    fn the_short_form_of_a_category_is_the_line_in_the_specification() {
        let line = render_short(&ExplainReport::for_category(Category::Snapshots), ColorPolicy::Never);

        assert_eq!(
            line,
            "snapshots  ·  REVIEW  ·  APFS local snapshots (Time Machine, OS update). Managed only \
             via tmutil; sizes not reported by macOS."
        );
        assert!(!line.contains('\n'), "--short is one line");
    }

    #[test]
    fn the_short_form_of_a_volume_names_it_its_role_and_its_size() {
        let line = render_short(&ExplainReport::for_volume(data_volume()), ColorPolicy::Never);

        assert_eq!(
            line,
            "disk3s5  ·  Data  ·  Macintosh HD - Data, 798.2 GB. The writable volume that holds \
             your home folder."
        );
    }

    #[test]
    fn the_short_form_of_a_path_leads_with_the_path() {
        let line =
            render_short(&ExplainReport::for_path(Path::new("/Users"), data_volume()), ColorPolicy::Never);

        assert!(line.starts_with("/Users  ·  Data  ·  on Macintosh HD - Data (disk3s5),"), "{line}");
    }

    #[test]
    fn every_paragraph_is_wrapped_and_indented() {
        let text = plain(&ExplainReport::for_category(Category::BuildCache));

        let body: Vec<&str> =
            text.lines().filter(|line| !line.is_empty() && !SECTIONS.contains(line)).collect();
        assert!(body.len() > 3, "the paragraphs must wrap: {text}");
        for line in body.iter().skip(1) {
            assert!(line.starts_with(PARAGRAPH_INDENT), "{line:?}");
            assert!(line.chars().count() <= 78, "{} chars: {line}", line.chars().count());
        }
    }

    #[test]
    fn color_decorates_the_header_and_changes_no_word() {
        let report = ExplainReport::for_category(Category::Trash);
        let coloured = render(&report, ColorPolicy::Always);

        assert!(coloured.contains('\u{1b}'), "{coloured:?}");
        assert!(coloured.contains("REVIEW"), "{coloured}");
        assert!(!plain(&report).contains('\u{1b}'));
    }

    #[test]
    fn every_category_renders_three_non_empty_sections() {
        for category in Category::all() {
            let text = plain(&ExplainReport::for_category(category));
            for title in SECTIONS {
                let body = text.split(title).nth(1).unwrap_or_default();
                let first = body.lines().nth(1).unwrap_or_default();
                assert!(first.trim().len() > 20, "{category}: `{title}` says nothing");
            }
            assert!(text.starts_with(category.as_str()), "{text}");
        }
    }
}
