//! The volumes table of one container (`docs/cli-spec.md` §3.1).
//!
//! Three aligned columns — name, role, used bytes — and a trailing note that
//! says, in words, what the row's colour only hints at: which volume holds the
//! user's data, which ones macOS keeps to itself, and which one Broza may not
//! write to even though its role would allow it.

use broza::model::{Container, Volume, VolumeRole};

use crate::output::human::role_style;
use crate::output::{ColorPolicy, format_bytes, paint};

/// Gap between a volume's columns.
const COLUMN_GAP: &str = "  ";
/// Minimum width of the volume-name column, so short names still line up.
const MIN_NAME_WIDTH: usize = 16;
/// Heading above the table.
const TABLE_HEADING: &str = "Volumes in this container:";

/// The volumes table of `container`, every line prefixed with `indent`.
pub fn render(container: &Container, indent: &str, policy: ColorPolicy) -> String {
    let widths = Widths::of(&container.volumes);
    let last = container.volumes.len().saturating_sub(1);
    let rows = container.volumes.iter().enumerate().map(|(index, volume)| {
        format!("{indent}{} {}", branch(index, last), render_volume(volume, &widths, policy))
    });
    std::iter::once(format!("{indent}{TABLE_HEADING}")).chain(rows).collect::<Vec<_>>().join("\n")
}

/// The box-drawing character that opens, continues or closes the table.
const fn branch(index: usize, last: usize) -> char {
    if index == last {
        '└'
    } else if index == 0 {
        '┌'
    } else {
        '├'
    }
}

/// Column widths shared by every row of one table.
struct Widths {
    /// Widest volume name.
    name: usize,
    /// Widest role label.
    role: usize,
    /// Widest formatted size.
    size: usize,
}

impl Widths {
    /// Measure `volumes`, never narrower than [`MIN_NAME_WIDTH`].
    fn of(volumes: &[Volume]) -> Self {
        let name = volumes.iter().map(|v| v.name.chars().count()).max().unwrap_or(0).max(MIN_NAME_WIDTH);
        let role = volumes.iter().map(|v| role_label(v.role).len()).max().unwrap_or(0);
        let size = volumes.iter().map(|v| format_bytes(v.used_bytes).len()).max().unwrap_or(0);
        Self { name, role, size }
    }
}

/// One row: name, role, used bytes, and what the user should know about it.
///
/// Padding is computed on the plain text and only then coloured, so a row is
/// the same width whether or not the terminal takes escapes.
fn render_volume(volume: &Volume, widths: &Widths, policy: ColorPolicy) -> String {
    let name = format!("{:<width$}", volume.name, width = widths.name);
    let role = format!("{:<width$}", role_label(volume.role), width = widths.role);
    let size = format!("{:>width$}", format_bytes(volume.used_bytes), width = widths.size);
    let style = role_style(volume.role);
    let row = format!(
        "{}{COLUMN_GAP}{}{COLUMN_GAP}{size}",
        paint(policy, style, &name),
        paint(policy, style, &role)
    );
    let note = volume_note(volume);
    if note.is_empty() { row } else { format!("{row}{COLUMN_GAP} {note}") }
}

/// Human label of a role, as `docs/cli-spec.md` §3.1 prints it.
pub const fn role_label(role: VolumeRole) -> &'static str {
    match role {
        VolumeRole::System => "System",
        VolumeRole::Data => "Data",
        VolumeRole::Preboot => "Preboot",
        VolumeRole::Recovery => "Recovery",
        VolumeRole::Vm => "VM",
        VolumeRole::Backup => "Backup",
        VolumeRole::User => "User",
        _ => "Unknown",
    }
}

/// The trailing note of a volume row: what it is, and whether Broza may write.
fn volume_note(volume: &Volume) -> String {
    let by_role = match volume.role {
        VolumeRole::System => "read-only, sealed",
        VolumeRole::Data => "← your data",
        VolumeRole::Vm => "swap",
        VolumeRole::Backup => "Time Machine",
        _ => "",
    };
    let unwritable = matches!(volume.role, VolumeRole::Data | VolumeRole::User) && !volume.writable_by_broza;
    match (by_role, unwritable) {
        ("", false) => String::new(),
        ("", true) => "not writable".to_owned(),
        (note, false) => note.to_owned(),
        (note, true) => format!("{note}, not writable"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;

    use broza::model::FsKind;

    use super::*;
    use crate::output::Style;

    fn volume(name: &str, role: VolumeRole, used_bytes: u64) -> Volume {
        Volume {
            id: "disk3s1".parse().unwrap_or_else(|e| panic!("{e}")),
            name: name.to_owned(),
            role,
            mount_point: Some(PathBuf::from("/")),
            used_bytes,
            writable_by_broza: role.writable_by_broza(),
            purpose: String::new(),
        }
    }

    fn container(volumes: Vec<Volume>) -> Container {
        Container {
            id: "disk3".parse().unwrap_or_else(|e| panic!("{e}")),
            kind: FsKind::Apfs,
            size_bytes: 1_000,
            used_bytes: 500,
            free_bytes: 500,
            purgeable_bytes: 0,
            volumes,
        }
    }

    fn boot() -> Container {
        container(vec![
            volume("Macintosh HD", VolumeRole::System, 17_116_610_560),
            volume("Data", VolumeRole::Data, 409_422_745_600),
            volume("VM", VolumeRole::Vm, 6_442_795_008),
        ])
    }

    fn plain(container: &Container) -> String {
        render(container, "   ", ColorPolicy::Never)
    }

    #[test]
    fn the_table_opens_continues_and_closes() {
        let text = plain(&boot());

        let prefixes: Vec<char> =
            text.lines().skip(1).filter_map(|line| line.trim_start().chars().next()).collect();
        assert_eq!(prefixes, vec!['┌', '├', '└']);
        assert!(text.starts_with("   Volumes in this container:"), "{text}");
    }

    #[test]
    fn a_single_volume_closes_the_table_on_its_own() {
        let text = plain(&container(vec![volume("Kiro CLI", VolumeRole::User, 1)]));
        assert_eq!(branch(0, 0), '└');
        assert!(text.lines().nth(1).is_some_and(|line| line.trim_start().starts_with('└')), "{text}");
    }

    #[test]
    fn every_column_lines_up_across_rows() {
        let text = plain(&boot());

        let rows: Vec<&str> = text.lines().skip(1).collect();
        let ends: Vec<usize> =
            rows.iter().map(|row| row.find(" GB").map_or(0, |index| index + " GB".len())).collect();
        assert!(ends.iter().all(|end| *end == ends[0] && *end > 0), "{ends:?}\n{text}");
    }

    #[test]
    fn each_role_carries_the_note_the_specification_gives_it() {
        let text = plain(&boot());

        assert!(text.contains("read-only, sealed"), "{text}");
        assert!(text.contains("← your data"), "{text}");
        assert!(text.contains("swap"), "{text}");
    }

    #[test]
    fn a_backup_volume_is_named_and_a_plain_user_volume_is_not() {
        assert_eq!(volume_note(&volume("TM", VolumeRole::Backup, 1)), "Time Machine");
        assert_eq!(volume_note(&volume("Ext", VolumeRole::User, 1)), "");
        assert_eq!(volume_note(&volume("Preboot", VolumeRole::Preboot, 1)), "");
    }

    #[test]
    fn a_role_that_may_write_but_cannot_says_so() {
        let mut data = volume("Data", VolumeRole::Data, 1);
        data.writable_by_broza = false;
        let mut user = volume("Ext", VolumeRole::User, 1);
        user.writable_by_broza = false;

        assert_eq!(volume_note(&data), "← your data, not writable");
        assert_eq!(volume_note(&user), "not writable");
    }

    #[test]
    fn a_protected_volume_never_claims_to_be_unwritable_twice() {
        // `system` is already "read-only, sealed"; adding "not writable" would
        // be noise, and the role note is the accurate one.
        assert_eq!(volume_note(&volume("Macintosh HD", VolumeRole::System, 1)), "read-only, sealed");
    }

    #[test]
    fn every_role_has_a_label_of_its_own() {
        let mut labels = [
            VolumeRole::System,
            VolumeRole::Data,
            VolumeRole::Preboot,
            VolumeRole::Recovery,
            VolumeRole::Vm,
            VolumeRole::Backup,
            VolumeRole::User,
            VolumeRole::Unknown,
        ]
        .map(role_label)
        .to_vec();
        labels.sort_unstable();
        let total = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), total);
    }

    #[test]
    fn colour_follows_the_shared_role_style_and_changes_no_word() {
        assert_eq!(role_style(VolumeRole::Data), Style::Bold);
        assert_eq!(role_style(VolumeRole::User), Style::Bold);
        assert_eq!(role_style(VolumeRole::System), Style::Dim);

        let coloured = render(&boot(), "   ", ColorPolicy::Always);
        assert!(coloured.contains('\u{1b}'));
        for word in ["Macintosh HD", "Data", "VM", "← your data"] {
            assert!(coloured.contains(word), "{word} must survive colouring");
        }
    }

    #[test]
    fn an_empty_container_still_produces_only_its_heading() {
        let text = plain(&container(Vec::new()));
        assert_eq!(text, "   Volumes in this container:");
    }
}
