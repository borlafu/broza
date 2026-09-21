//! The human rendering of `broza scan` (`docs/cli-spec.md` §3.1).
//!
//! The mandatory rules of that section are structural, not cosmetic, and they
//! are what the tests at the bottom pin:
//!
//! - purgeable space is **always** on its own line and is never added to free
//!   space (`AGENTS.md` §2.7);
//! - the percentage and the bar are computed at **container** level, because
//!   APFS volumes share the container's space;
//! - every colour carries a word as well, so `--no-color` loses decoration and
//!   no information (RNF-06).

use std::fmt::Write as _;

use broza::model::{Container, Disk, FsKind, ScanReport, Volume, VolumeRole};

use crate::output::bar::{Fullness, fullness, usage_bar, usage_percentage};
use crate::output::{ColorPolicy, Style, format_bytes, paint};

/// Indent of everything below a physical disk.
const CONTAINER_INDENT: &str = "   ";
/// Width of the `Used` / `Free` / `Purgeable` labels.
const CAPACITY_LABEL_WIDTH: usize = 11;
/// Gap between a volume's columns.
const COLUMN_GAP: &str = "  ";
/// Minimum width of the volume-name column, so short names still line up.
const MIN_NAME_WIDTH: usize = 16;
/// The label that keeps `purgeable` honest (`docs/cli-spec.md` §3.1).
const PURGEABLE_NOTE: &str = "← macOS shows this as \"available\"";
/// What a scan of a machine with no disks says.
const NO_DISKS: &str = "No disks found.";

/// Render `report` for a terminal.
pub fn render(report: &ScanReport, policy: ColorPolicy) -> String {
    if report.disks.is_empty() {
        return NO_DISKS.to_owned();
    }
    report.disks.iter().map(|disk| render_disk(disk, policy)).collect::<Vec<_>>().join("\n\n")
}

/// One physical disk and every container on it.
fn render_disk(disk: &Disk, policy: ColorPolicy) -> String {
    let external = if disk.internal { String::new() } else { "  (external)".to_owned() };
    let mut text = format!(
        "Physical disk  {}  —  {}  ({}){external}",
        disk.id,
        disk.model,
        format_bytes(disk.size_bytes)
    );
    let last = disk.containers.len().saturating_sub(1);
    for (index, container) in disk.containers.iter().enumerate() {
        let branch = if index == last { "└─" } else { "├─" };
        let _ignored = write!(text, "\n{}", render_container(container, branch, policy));
    }
    text
}

/// One container: its header, its capacity block and its volumes.
fn render_container(container: &Container, branch: &str, policy: ColorPolicy) -> String {
    let mut text = format!(
        "{branch} {}  {}  ({})",
        container_kind(&container.kind),
        container.id,
        format_bytes(container.size_bytes)
    );
    let _ignored = write!(text, "\n{}", render_capacity(container, policy));
    if !container.volumes.is_empty() {
        let _ignored = write!(text, "\n\n{}", render_volumes(container, policy));
    }
    text
}

/// How a container is announced, by filesystem family.
fn container_kind(kind: &FsKind) -> String {
    match kind {
        FsKind::Apfs => "APFS container".to_owned(),
        FsKind::HfsPlus => "HFS+ volume".to_owned(),
        other => format!("{other} container"),
    }
}

/// The three capacity lines, purgeable always on its own.
fn render_capacity(container: &Container, policy: ColorPolicy) -> String {
    let sizes = [container.used_bytes, container.free_bytes, container.purgeable_bytes].map(format_bytes);
    let width = sizes.iter().map(String::len).max().unwrap_or(0);
    let bar = usage_bar(container.used_bytes, container.size_bytes);
    let percentage = usage_percentage(container.used_bytes, container.size_bytes);
    let painted_bar =
        paint(policy, fullness_style(fullness(container.used_bytes, container.size_bytes)), &bar);
    format!(
        "{CONTAINER_INDENT}├─ {:<CAPACITY_LABEL_WIDTH$}{:>width$}  {painted_bar}  {percentage}\n\
         {CONTAINER_INDENT}├─ {:<CAPACITY_LABEL_WIDTH$}{:>width$}\n\
         {CONTAINER_INDENT}└─ {:<CAPACITY_LABEL_WIDTH$}{:>width$}  {PURGEABLE_NOTE}",
        "Used", sizes[0], "Free", sizes[1], "Purgeable", sizes[2],
    )
}

/// Colour of the usage bar at a given fill level.
const fn fullness_style(level: Fullness) -> Style {
    match level {
        Fullness::Comfortable => Style::Green,
        Fullness::Busy => Style::Yellow,
        Fullness::Full => Style::Red,
    }
}

/// The volumes table of one container.
fn render_volumes(container: &Container, policy: ColorPolicy) -> String {
    let widths = Widths::of(&container.volumes);
    let last = container.volumes.len().saturating_sub(1);
    let rows = container.volumes.iter().enumerate().map(|(index, volume)| {
        let prefix = if index == last {
            '└'
        } else if index == 0 {
            '┌'
        } else {
            '├'
        };
        format!("{CONTAINER_INDENT}{prefix} {}", render_volume(volume, &widths, policy))
    });
    let header = format!("{CONTAINER_INDENT}Volumes in this container:");
    std::iter::once(header).chain(rows).collect::<Vec<_>>().join("\n")
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
fn render_volume(volume: &Volume, widths: &Widths, policy: ColorPolicy) -> String {
    let name = format!("{:<width$}", volume.name, width = widths.name);
    let role = format!("{:<width$}", role_label(volume.role), width = widths.role);
    let size = format!("{:>width$}", format_bytes(volume.used_bytes), width = widths.size);
    let note = volume_note(volume);
    let row = format!(
        "{}{COLUMN_GAP}{}{COLUMN_GAP}{size}",
        paint(policy, name_style(volume.role), &name),
        paint(policy, role_style(volume.role), &role)
    );
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

/// The user's own data is emphasised; everything Broza may not write to is not.
const fn name_style(role: VolumeRole) -> Style {
    if matches!(role, VolumeRole::Data) { Style::Bold } else { Style::Dim }
}

/// Protected roles are dimmed, the data role is emphasised, the rest is plain.
const fn role_style(role: VolumeRole) -> Style {
    match role {
        VolumeRole::Data => Style::Bold,
        _ => Style::Dim,
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

    use broza::model::VolumeId;

    use super::*;

    fn id(raw: &str) -> VolumeId {
        raw.parse().unwrap_or_else(|e| panic!("{raw}: {e}"))
    }

    fn volume(raw_id: &str, name: &str, role: VolumeRole, used_bytes: u64) -> Volume {
        Volume {
            id: id(raw_id),
            name: name.to_owned(),
            role,
            mount_point: Some(PathBuf::from("/")),
            used_bytes,
            writable_by_broza: role.writable_by_broza(),
            purpose: String::new(),
        }
    }

    /// The machine of the sketch in `docs/cli-spec.md` §3.1.
    fn sketch() -> ScanReport {
        ScanReport {
            disks: vec![Disk {
                id: id("disk0"),
                model: "APPLE SSD AP1024Z".to_owned(),
                size_bytes: 1_000_555_581_440,
                internal: true,
                containers: vec![Container {
                    id: id("disk3"),
                    kind: FsKind::Apfs,
                    size_bytes: 994_662_584_320,
                    used_bytes: 812_400_000_000,
                    free_bytes: 98_120_000_000,
                    purgeable_bytes: 84_140_000_000,
                    volumes: vec![
                        volume("disk3s1", "Macintosh HD", VolumeRole::System, 11_300_000_000),
                        volume("disk3s5", "Macintosh HD - Data", VolumeRole::Data, 798_210_000_000),
                        volume("disk3s2", "Preboot", VolumeRole::Preboot, 6_520_000_000),
                        volume("disk3s3", "Recovery", VolumeRole::Recovery, 1_200_000_000),
                        volume("disk3s6", "VM", VolumeRole::Vm, 3_000_000_000),
                    ],
                }],
            }],
            largest_items: Vec::new(),
        }
    }

    fn plain(report: &ScanReport) -> String {
        render(report, ColorPolicy::Never)
    }

    #[test]
    fn the_header_names_the_disk_its_model_and_its_size() {
        let text = plain(&sketch());
        assert!(text.starts_with("Physical disk  disk0  —  APPLE SSD AP1024Z  (1.00 TB)"), "{text}");
        assert!(text.contains("└─ APFS container  disk3  (994.7 GB)"), "{text}");
    }

    #[test]
    fn purgeable_is_on_its_own_line_and_never_summed_into_free() {
        let text = plain(&sketch());

        let purgeable = text.lines().find(|line| line.contains("Purgeable")).unwrap_or_default();
        assert!(purgeable.contains("84.1 GB"), "{purgeable}");
        assert!(purgeable.contains(PURGEABLE_NOTE), "{purgeable}");
        let free = text.lines().find(|line| line.contains("Free")).unwrap_or_default();
        assert!(free.contains("98.1 GB"), "{free}");
        assert!(!free.contains("Purgeable"), "free and purgeable share no line: {free}");
        // 98.12 + 84.14 = 182.3 GB, the number Broza must never print.
        assert!(!text.contains("182.3 GB"), "purgeable was summed into free");
    }

    #[test]
    fn the_bar_and_the_percentage_are_computed_at_container_level() {
        let text = plain(&sketch());

        let used = text.lines().find(|line| line.contains("Used")).unwrap_or_default();
        assert!(used.contains("████████████████░░░░"), "{used}");
        assert!(used.contains("81.7%"), "{used}");
        // 812.40 of the *container*, not of the 1.00 TB disk (81.2%).
        assert!(!used.contains("81.2%"), "{used}");
    }

    #[test]
    fn the_bar_is_always_twenty_cells_whatever_the_fill() {
        for used in [0, 1, 500_000_000_000, 994_662_584_320] {
            let mut report = sketch();
            report.disks[0].containers[0].used_bytes = used;
            let text = plain(&report);
            let line = text.lines().find(|line| line.contains("Used")).unwrap_or_default();
            let cells = line.chars().filter(|c| *c == '█' || *c == '░').count();
            assert_eq!(cells, 20, "{used}: {line}");
        }
    }

    #[test]
    fn every_volume_gets_a_row_with_its_role_and_its_note() {
        let text = plain(&sketch());

        assert!(text.contains("Volumes in this container:"), "{text}");
        assert!(text.contains("┌ Macintosh HD  "), "{text}");
        assert!(text.contains("read-only, sealed"), "{text}");
        assert!(text.contains("← your data"), "{text}");
        assert!(text.contains("swap"), "{text}");
        assert!(text.lines().any(|line| line.trim_start().starts_with("└ VM")), "{text}");
    }

    /// Volume rows, which begin with `┌`, `├` or `└` and no `─`.
    fn volume_rows(text: &str) -> Vec<&str> {
        text.lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                trimmed.starts_with(['┌', '├', '└']) && !trimmed.contains('─')
            })
            .collect()
    }

    #[test]
    fn the_volume_columns_line_up() {
        let text = plain(&sketch());

        let rows = volume_rows(&text);
        assert_eq!(rows.len(), 5, "{text}");
        let size_ends: Vec<usize> =
            rows.iter().map(|row| row.find(" GB").map_or(0, |index| index + " GB".len())).collect();
        assert!(size_ends.iter().all(|end| *end == size_ends[0]), "{size_ends:?}\n{text}");
    }

    #[test]
    fn an_unwritable_data_volume_says_so() {
        let mut report = sketch();
        report.disks[0].containers[0].volumes[1].writable_by_broza = false;

        let text = plain(&report);

        assert!(text.contains("← your data, not writable"), "{text}");
    }

    #[test]
    fn an_external_disk_is_marked_and_an_hfs_partition_is_named_a_volume() {
        let report = ScanReport {
            disks: vec![Disk {
                id: id("disk4"),
                model: "Disk Image".to_owned(),
                size_bytes: 2_000_000_000,
                internal: false,
                containers: vec![Container {
                    id: id("disk4s1"),
                    kind: FsKind::HfsPlus,
                    size_bytes: 2_000_000_000,
                    used_bytes: 1_000_000_000,
                    free_bytes: 1_000_000_000,
                    purgeable_bytes: 0,
                    volumes: vec![volume("disk4s1", "Backup Drive", VolumeRole::User, 1_000_000_000)],
                }],
            }],
            largest_items: Vec::new(),
        };

        let text = plain(&report);

        assert!(text.contains("(external)"), "{text}");
        assert!(text.contains("└─ HFS+ volume  disk4s1"), "{text}");
        assert!(text.contains("Purgeable"), "the rule holds for HFS+ too: {text}");
    }

    #[test]
    fn an_unknown_filesystem_keeps_its_own_token() {
        assert_eq!(container_kind(&FsKind::Unknown("zfs".into())), "zfs container");
    }

    #[test]
    fn a_machine_with_no_disks_says_so_instead_of_printing_nothing() {
        assert_eq!(plain(&ScanReport::default()), NO_DISKS);
    }

    #[test]
    fn color_adds_escapes_and_removes_no_words() {
        let with_color = render(&sketch(), ColorPolicy::Always);
        let without = plain(&sketch());

        assert!(with_color.contains('\u{1b}'), "colour must actually be emitted");
        assert!(!without.contains('\u{1b}'), "{without}");
        for word in ["System", "Data", "81.7%", "read-only, sealed", "← your data"] {
            assert!(with_color.contains(word), "{word} must survive colouring");
            assert!(without.contains(word), "{word} must survive --no-color");
        }
    }

    #[test]
    fn every_role_has_a_label_and_the_protected_ones_are_dimmed() {
        for role in [
            VolumeRole::System,
            VolumeRole::Preboot,
            VolumeRole::Recovery,
            VolumeRole::Vm,
            VolumeRole::Backup,
            VolumeRole::Unknown,
        ] {
            assert!(!role_label(role).is_empty(), "{role:?}");
            assert_eq!(role_style(role), Style::Dim, "{role:?}");
        }
        assert_eq!(role_style(VolumeRole::Data), Style::Bold);
    }

    #[test]
    fn two_containers_on_one_disk_are_both_branched() {
        let mut report = sketch();
        let mut second = report.disks[0].containers[0].clone();
        second.id = id("disk5");
        report.disks[0].containers.push(second);

        let text = plain(&report);

        assert!(text.contains("├─ APFS container  disk3"), "{text}");
        assert!(text.contains("└─ APFS container  disk5"), "{text}");
    }
}
