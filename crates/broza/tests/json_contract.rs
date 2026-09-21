//! Round-trip tests for every JSON example in `docs/cli-spec.md` §4.
//!
//! Each fixture under `tests/fixtures/json/` is copied verbatim from the specification.
//! A fixture is deserialised into its model type, serialised again, compared with the
//! original document and snapshotted. Any change to `crates/broza/src/model/` that is
//! not reflected in the specification breaks these tests.

use std::path::Path;

use broza::model::{
    CleanPlan, Envelope, Finding, FsKind, QuarantineList, ReclaimReport, RestoreReport, ScanReport,
    SessionState, SuggestReport, VolumeRole,
};
use serde::{Serialize, de::DeserializeOwned};

/// Wrapper for the fixtures that only show the `data` member of the envelope.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct DataOnly<T> {
    /// Command-specific payload.
    data: T,
}

/// Read a fixture as a JSON value.
fn fixture(name: &str) -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/json").join(name);
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Deserialise a fixture into `T`, serialise it again and assert the document is unchanged.
fn round_trip<T: DeserializeOwned + Serialize>(name: &str) -> serde_json::Value {
    let original = fixture(name);
    let parsed: T = serde_json::from_value(original.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
    let reserialized = serde_json::to_value(&parsed).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(reserialized, original, "{name} does not round-trip");
    reserialized
}

#[test]
fn envelope_4_1_round_trips() {
    let value = round_trip::<Envelope<serde_json::Value>>("4_1_envelope.json");
    insta::assert_json_snapshot!("envelope_4_1", value);
}

#[test]
fn scan_4_2_round_trips() {
    let value = round_trip::<DataOnly<ScanReport>>("4_2_scan.json");
    insta::assert_json_snapshot!("scan_4_2", value);
}

#[test]
fn suggest_4_3_round_trips() {
    let value = round_trip::<DataOnly<SuggestReport>>("4_3_suggest.json");
    insta::assert_json_snapshot!("suggest_4_3", value);
}

#[test]
fn clean_4_4_round_trips() {
    let value = round_trip::<DataOnly<CleanPlan>>("4_4_clean.json");
    insta::assert_json_snapshot!("clean_4_4", value);
}

#[test]
fn quarantine_list_4_5_round_trips() {
    let value = round_trip::<DataOnly<QuarantineList>>("4_5_quarantine_list.json");
    insta::assert_json_snapshot!("quarantine_list_4_5", value);
}

#[test]
fn restore_4_6_round_trips() {
    let value = round_trip::<DataOnly<RestoreReport>>("4_6_restore.json");
    insta::assert_json_snapshot!("restore_4_6", value);
}

#[test]
fn purge_4_6_round_trips() {
    let value = round_trip::<DataOnly<ReclaimReport>>("4_6_purge.json");
    insta::assert_json_snapshot!("purge_4_6", value);
}

#[test]
fn every_finding_in_the_specification_satisfies_its_invariants() {
    let report: DataOnly<SuggestReport> =
        serde_json::from_value(fixture("4_3_suggest.json")).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(report.data.findings.len(), 3);
    for finding in &report.data.findings {
        if let Err(error) = finding.validate() {
            panic!("{}: {error}", finding.id());
        }
    }
}

#[test]
fn the_clean_plan_in_the_specification_satisfies_its_invariants() {
    let plan: DataOnly<CleanPlan> =
        serde_json::from_value(fixture("4_4_clean.json")).unwrap_or_else(|e| panic!("{e}"));
    assert!(plan.data.validate().is_ok());
    let sum: u64 = plan.data.items().iter().map(|item| item.size_bytes).sum();
    assert_eq!(plan.data.planned_bytes(), sum);
    assert!(!plan.data.is_dry_run());
    assert_eq!(plan.data.items().len(), 2);
}

#[test]
fn unknown_fields_are_ignored() {
    let mut value = fixture("4_2_scan.json");
    let data = value.get_mut("data").unwrap_or_else(|| panic!("missing data"));
    data["future_total_bytes"] = serde_json::json!(42);
    data["disks"][0]["future_flag"] = serde_json::json!(true);
    data["disks"][0]["containers"][0]["volumes"][0]["future_purpose"] = serde_json::json!("x");
    let parsed: DataOnly<ScanReport> = serde_json::from_value(value).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(parsed.data.disks.len(), 1);
}

#[test]
fn an_unknown_role_collapses_to_the_unknown_variant() {
    let mut value = fixture("4_2_scan.json");
    value["data"]["disks"][0]["containers"][0]["volumes"][0]["role"] = serde_json::json!("nursery");
    let parsed: DataOnly<ScanReport> = serde_json::from_value(value).unwrap_or_else(|e| panic!("{e}"));
    let volume = &parsed.data.disks[0].containers[0].volumes[0];
    assert_eq!(volume.role, VolumeRole::Unknown);
    assert!(!volume.role.writable_by_broza());
}

#[test]
fn a_persisted_enum_value_from_a_newer_broza_survives_a_round_trip() {
    let mut scan = fixture("4_2_scan.json");
    scan["data"]["disks"][0]["containers"][0]["type"] = serde_json::json!("zfs");
    let parsed: DataOnly<ScanReport> = serde_json::from_value(scan.clone()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(parsed.data.disks[0].containers[0].kind, FsKind::Unknown("zfs".into()));
    assert_eq!(serde_json::to_value(&parsed).unwrap_or_else(|e| panic!("{e}")), scan);

    let mut list = fixture("4_5_quarantine_list.json");
    list["data"]["sessions"][0]["state"] = serde_json::json!("archived");
    let parsed: DataOnly<QuarantineList> =
        serde_json::from_value(list.clone()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(parsed.data.sessions[0].state, SessionState::Unknown("archived".into()));
    assert_eq!(serde_json::to_value(&parsed).unwrap_or_else(|e| panic!("{e}")), list);
}

#[test]
fn an_unknown_closed_enum_value_is_an_error_and_never_a_panic() {
    let mut value = fixture("4_3_suggest.json");
    value["data"]["findings"][0]["risk"] = serde_json::json!("chartreuse");
    assert!(serde_json::from_value::<DataOnly<SuggestReport>>(value).is_err());

    let mut value = fixture("4_4_clean.json");
    value["data"]["items"][0]["action"] = serde_json::json!("teleport");
    assert!(serde_json::from_value::<DataOnly<CleanPlan>>(value).is_err());
}

#[test]
fn a_malformed_identifier_is_rejected_at_the_boundary() {
    let mut value = fixture("4_3_suggest.json");
    value["data"]["findings"][0]["id"] = serde_json::json!("Build Cache/Xcode");
    assert!(serde_json::from_value::<DataOnly<SuggestReport>>(value).is_err());
}

#[test]
fn a_finding_that_contradicts_the_invariants_cannot_be_parsed() {
    let cloud_synced = 2;
    let mut value = fixture("4_3_suggest.json");
    value["data"]["findings"][cloud_synced]["actionable"] = serde_json::json!(true);
    value["data"]["findings"][cloud_synced]["action"] = serde_json::json!("purge");
    assert!(
        serde_json::from_value::<DataOnly<SuggestReport>>(value).is_err(),
        "a cloud-synced finding must never parse as actionable"
    );

    let mut value = fixture("4_3_suggest.json");
    value["data"]["findings"][cloud_synced]["instructions"] = serde_json::Value::Null;
    assert!(
        serde_json::from_value::<DataOnly<SuggestReport>>(value).is_err(),
        "an inform-only finding must carry the provider's instructions"
    );
}

#[test]
fn a_clean_plan_that_contradicts_the_invariants_cannot_be_parsed() {
    let mut value = fixture("4_4_clean.json");
    value["data"]["dry_run"] = serde_json::json!(true);
    assert!(
        serde_json::from_value::<DataOnly<CleanPlan>>(value).is_err(),
        "a dry run must not report quarantined or reclaimed bytes"
    );

    let mut value = fixture("4_4_clean.json");
    value["data"]["planned_bytes"] = serde_json::json!(1);
    assert!(
        serde_json::from_value::<DataOnly<CleanPlan>>(value).is_err(),
        "`planned_bytes` must be the sum of the item sizes"
    );

    let mut value = fixture("4_4_clean.json");
    value["data"]["items"][0]["error"] = serde_json::json!("collision");
    assert!(
        serde_json::from_value::<DataOnly<CleanPlan>>(value).is_err(),
        "a quarantined item must not carry an error"
    );
}

#[test]
fn findings_can_be_rebuilt_from_the_specification_examples() {
    let report: DataOnly<SuggestReport> =
        serde_json::from_value(fixture("4_3_suggest.json")).unwrap_or_else(|e| panic!("{e}"));
    let rebuilt: Vec<Finding> = report
        .data
        .findings
        .iter()
        .map(|finding| {
            let builder = Finding::builder(finding.id().clone(), finding.category(), finding.title())
                .action(finding.action())
                .risk(finding.risk());
            let builder = match finding.instructions() {
                Some(instructions) => builder.instructions(instructions.clone()),
                None => builder,
            };
            builder.build().unwrap_or_else(|e| panic!("{e}"))
        })
        .collect();
    for (original, rebuilt) in report.data.findings.iter().zip(&rebuilt) {
        assert_eq!(original.is_actionable(), rebuilt.is_actionable(), "{}", original.id());
        assert_eq!(original.action(), rebuilt.action(), "{}", original.id());
        assert_eq!(original.risk(), rebuilt.risk(), "{}", original.id());
    }
}
