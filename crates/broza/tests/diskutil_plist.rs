//! The `diskutil` plist parsers against output recorded on a real machine.
//!
//! The fixtures under `tests/fixtures/plist/macos26/` were captured and redacted
//! by `scripts/capture-diskutil-fixtures.sh`. Snapshotting what the parsers make
//! of them is how Broza notices that a macOS release changed the shape of the
//! output (`docs/implementation-plan.md` §3.6).

use broza::adapters::diskutil::{parse_apfs_list, parse_info, parse_list, parse_snapshots};
use broza::testing::FakeRunner;

/// Fixture directory of the macOS major these snapshots belong to.
const MACOS_MAJOR: &str = "macos26";

fn fixture(name: &str) -> Vec<u8> {
    let path = FakeRunner::fixture_path(&format!("plist/{MACOS_MAJOR}/{name}"));
    std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn diskutil_list_parses_into_disks_partitions_and_apfs_volumes() {
    let parsed = parse_list(&fixture("list.plist")).unwrap_or_else(|e| panic!("{e}"));

    insta::assert_debug_snapshot!(parsed);
}

#[test]
fn diskutil_apfs_list_parses_into_containers_with_roles_and_capacities() {
    let parsed = parse_apfs_list(&fixture("apfs_list.plist")).unwrap_or_else(|e| panic!("{e}"));

    insta::assert_debug_snapshot!(parsed);
}

#[test]
fn diskutil_info_of_a_physical_disk_reports_the_model_and_the_bus() {
    let parsed = parse_info(&fixture("info_disk0.plist")).unwrap_or_else(|e| panic!("{e}"));

    insta::assert_debug_snapshot!(parsed);
}

#[test]
fn diskutil_info_of_the_boot_volume_reports_its_container_and_seal() {
    let parsed = parse_info(&fixture("info_root.plist")).unwrap_or_else(|e| panic!("{e}"));

    insta::assert_debug_snapshot!(parsed);
}

#[test]
fn diskutil_info_of_the_data_volume_reports_its_mount_point() {
    let parsed = parse_info(&fixture("info_data.plist")).unwrap_or_else(|e| panic!("{e}"));

    insta::assert_debug_snapshot!(parsed);
}

#[test]
fn diskutil_info_of_the_preboot_volume_parses_like_any_other_volume() {
    let parsed = parse_info(&fixture("info_preboot.plist")).unwrap_or_else(|e| panic!("{e}"));

    insta::assert_debug_snapshot!(parsed);
}

#[test]
fn diskutil_info_of_a_disk_image_reports_no_solid_state_key() {
    let parsed = parse_info(&fixture("info_disk4.plist")).unwrap_or_else(|e| panic!("{e}"));

    assert!(!parsed.solid_state, "a disk image has no SolidState key and must default to false");
    insta::assert_debug_snapshot!(parsed);
}

#[test]
fn the_system_volume_snapshots_are_the_os_update_ones_and_none_is_purgeable() {
    let parsed =
        parse_snapshots(&fixture("apfs_list_snapshots_system.plist")).unwrap_or_else(|e| panic!("{e}"));

    assert!(parsed.iter().all(|snapshot| !snapshot.purgeable));
    insta::assert_debug_snapshot!(parsed);
}

#[test]
fn a_volume_without_snapshots_parses_into_an_empty_list() {
    let parsed =
        parse_snapshots(&fixture("apfs_list_snapshots_data.plist")).unwrap_or_else(|e| panic!("{e}"));

    assert!(parsed.is_empty());
}
