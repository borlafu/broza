//! The human rendering of `broza scan` (`docs/cli-spec.md` §3.1).
//!
//! The mandatory rules of that section are structural, not cosmetic, and they
//! are what the tests at the bottom pin:
//!
//! - purgeable space is **always** on its own line, labelled an estimate, and
//!   never added to free space (`AGENTS.md` §2.7);
//! - the percentage and the bar are computed at **container** level, because
//!   APFS volumes share the container's space;
//! - every colour carries a word as well, so `--no-color` loses decoration and
//!   no information (RNF-06).
//!
//! The volumes table of each container lives in [`volumes`].

pub mod volumes;

use std::fmt::Write as _;

use broza::model::{Container, Disk, FsKind, ScanReport};

use crate::output::bar::{Fullness, fullness, usage_bar, usage_percentage};
use crate::output::{ColorPolicy, Style, format_bytes, paint};

pub use volumes::role_label;

/// Indent of everything below the last container of a disk.
const LAST_INDENT: &str = "   ";
/// Indent of everything below a container that still has siblings.
const CONTINUED_INDENT: &str = "│  ";
/// Width of the `Used` / `Free` / `Purgeable` labels.
const CAPACITY_LABEL_WIDTH: usize = 12;
/// The label that keeps `purgeable` honest (`docs/cli-spec.md` §3.1).
const PURGEABLE_NOTE: &str = "← estimate; macOS shows this as \"available\"";
/// Marker of a disk macOS does not report as internal.
const EXTERNAL_MARKER: &str = "  (external)";
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
    let external = if disk.internal { "" } else { EXTERNAL_MARKER };
    let mut text = format!(
        "Physical disk  {}  —  {}  ({}){external}",
        disk.id,
        disk.model,
        format_bytes(disk.size_bytes)
    );
    let last = disk.containers.len().saturating_sub(1);
    for (index, container) in disk.containers.iter().enumerate() {
        let is_last = index == last;
        let branch = if is_last { "└─" } else { "├─" };
        let indent = if is_last { LAST_INDENT } else { CONTINUED_INDENT };
        let _ignored = write!(text, "\n{}", render_container(container, branch, indent, policy));
    }
    text
}

/// One container: its header, its capacity block and its volumes.
///
/// `indent` is the gutter everything below the header sits in: a continuation
/// bar while the disk has more containers to show, plain spaces for the last.
fn render_container(container: &Container, branch: &str, indent: &str, policy: ColorPolicy) -> String {
    let mut text = format!(
        "{branch} {}  {}  ({})",
        container_kind(&container.kind),
        container.id,
        format_bytes(container.size_bytes)
    );
    let _ignored = write!(text, "\n{}", render_capacity(container, indent, policy));
    if !container.volumes.is_empty() {
        let _ignored =
            write!(text, "\n{}\n{}", indent.trim_end(), volumes::render(container, indent, policy));
    }
    text
}

/// How a container is announced, by filesystem family.
pub fn container_kind(kind: &FsKind) -> String {
    match kind {
        FsKind::Apfs => "APFS container".to_owned(),
        FsKind::HfsPlus => "HFS+ volume".to_owned(),
        other => format!("{other} container"),
    }
}

/// The three capacity lines, purgeable always on its own.
fn render_capacity(container: &Container, indent: &str, policy: ColorPolicy) -> String {
    let sizes = [container.used_bytes, container.free_bytes, container.purgeable_bytes].map(format_bytes);
    let width = sizes.iter().map(String::len).max().unwrap_or(0);
    let bar = usage_bar(container.used_bytes, container.size_bytes);
    let level = fullness(container.used_bytes, container.size_bytes);
    let painted_bar = paint(policy, fullness_style(level), &bar);
    let percentage = usage_percentage(container.used_bytes, container.size_bytes);
    format!(
        "{indent}├─ {:<CAPACITY_LABEL_WIDTH$}{:>width$}  {painted_bar}  {percentage}\n\
         {indent}├─ {:<CAPACITY_LABEL_WIDTH$}{:>width$}\n\
         {indent}└─ {:<CAPACITY_LABEL_WIDTH$}{:>width$}  {PURGEABLE_NOTE}",
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;

    use broza::model::{Volume, VolumeId, VolumeRole};

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
    fn purgeable_is_on_its_own_line_labelled_an_estimate_and_never_summed() {
        let text = plain(&sketch());

        let purgeable = text.lines().find(|line| line.contains("Purgeable")).unwrap_or_default();
        assert!(purgeable.contains("84.1 GB"), "{purgeable}");
        assert!(purgeable.contains("estimate"), "{purgeable}");
        assert!(purgeable.contains("available"), "{purgeable}");
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
    fn the_capacity_labels_all_start_in_the_same_column() {
        let text = plain(&sketch());

        let sizes: Vec<usize> = ["Used", "Free", "Purgeable"]
            .iter()
            .filter_map(|label| text.lines().find(|line| line.contains(*label)))
            .map(|line| line.find(" GB").unwrap_or_default())
            .collect();
        assert_eq!(sizes.len(), 3);
        assert!(sizes.iter().all(|end| *end == sizes[0]), "{sizes:?}\n{text}");
    }

    #[test]
    fn the_volumes_table_is_delegated_and_present() {
        let text = plain(&sketch());

        assert!(text.contains("Volumes in this container:"), "{text}");
        assert!(text.contains("┌ Macintosh HD  "), "{text}");
        assert!(text.lines().any(|line| line.trim_start().starts_with("└ VM")), "{text}");
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
    fn a_container_with_siblings_is_drawn_inside_a_continuation_gutter() {
        let mut report = sketch();
        let mut second = report.disks[0].containers[0].clone();
        second.id = id("disk5");
        report.disks[0].containers.push(second);

        let text = plain(&report);

        assert!(text.contains("├─ APFS container  disk3"), "{text}");
        assert!(text.contains("└─ APFS container  disk5"), "{text}");
        let first_used = text.lines().find(|line| line.contains("Used")).unwrap_or_default();
        assert!(first_used.starts_with(CONTINUED_INDENT), "{first_used:?}");
        let used_lines: Vec<&str> = text.lines().filter(|line| line.contains("Used")).collect();
        let last_used = used_lines.last().copied().unwrap_or_default();
        assert!(last_used.starts_with(LAST_INDENT), "{last_used:?}");
        // The gutter runs unbroken down to the last container's header.
        let gutter = text.lines().filter(|line| line.starts_with('│')).count();
        assert!(gutter >= 9, "{text}");
    }

    #[test]
    fn a_single_container_needs_no_gutter_at_all() {
        let text = plain(&sketch());
        assert!(!text.contains('│'), "{text}");
    }
}
