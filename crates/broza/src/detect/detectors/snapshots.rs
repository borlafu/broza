//! `snapshots`: APFS local snapshots Time Machine left behind (`docs/cli-spec.md` §3.3).
//!
//! One finding, `snapshots.timemachine-local`, amber and `tmutil_delete`. macOS
//! reports no size for a snapshot, so `reclaimable_bytes` is `0` and the
//! reasoning says so; the finding lists every purgeable `com.apple.TimeMachine.*`
//! snapshot with the volume it belongs to, which is what a `clean` plan needs
//! to name it. `com.apple.os.update-*` snapshots are never listed: they are not
//! purgeable and the pending update needs them. A volume whose snapshots
//! cannot be listed is a `location_unreadable` warning, never a failure.

use crate::BrozaError;
use crate::model::{Category, Snapshot, VolumeRole};

use super::support::start;
use crate::detect::detector::{DetectContext, Detected, Detector};

/// Why the size is zero, word for word as the specification asks.
const NO_SIZE_REASON: &str = "size not reported by macOS";

/// The `snapshots` detector.
pub struct Snapshots;

impl Detector for Snapshots {
    fn category(&self) -> Category {
        Category::Snapshots
    }

    fn detect(&self, context: &DetectContext<'_>) -> Result<Detected, BrozaError> {
        let mut detected = Detected::default();
        let mut listed: Vec<Snapshot> = Vec::new();
        for entry in context.mounts.entries().iter().filter(|entry| holds_local_snapshots(entry.volume.role))
        {
            match context.snapshots.list(&entry.volume.id) {
                Ok(snapshots) => listed.extend(snapshots.into_iter().filter_map(|snapshot| {
                    let located = Snapshot {
                        volume: Some(entry.volume.id.clone()),
                        mount_point: Some(entry.mount_point.clone()),
                        ..snapshot
                    };
                    located.is_actionable().then_some(located)
                })),
                Err(error) => detected.warnings.push(Detected::unreadable(
                    Category::Snapshots,
                    &entry.mount_point,
                    "its local snapshots",
                    &error,
                )),
            }
        }
        if listed.is_empty() {
            return Ok(detected);
        }
        listed.sort_by(|a, b| a.name.cmp(&b.name));
        let count = u64::try_from(listed.len()).unwrap_or(u64::MAX);
        let finding = start(Category::Snapshots, "timemachine-local", "Time Machine local snapshots")?
            .description("APFS copy-on-write snapshots kept locally by Time Machine.")
            .reasoning(NO_SIZE_REASON)
            .item_count(count)
            .snapshots(listed)
            .build()?;
        Ok(detected.with_finding(Some(finding)))
    }
}

/// Local snapshots live on the volumes users write to; a protected volume's
/// snapshots are the OS update's and are never Broza's business.
const fn holds_local_snapshots(role: VolumeRole) -> bool {
    matches!(role, VolumeRole::Data | VolumeRole::User)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::detect::test_support::{context_over, home};
    use crate::model::{Action, Risk, VolumeId};
    use crate::testing::FakeFileOps;

    fn snapshot(name: &str, purgeable: bool) -> Snapshot {
        Snapshot {
            name: name.into(),
            uuid: Some("00000021-1111-4222-8333-000000000021".into()),
            purgeable,
            volume: None,
            mount_point: None,
        }
    }

    fn data() -> VolumeId {
        "disk3s5".parse().unwrap()
    }

    #[test]
    fn purgeable_time_machine_snapshots_are_listed_with_their_volume_and_no_size() {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        fs.add_dir(home());
        let world = context_over(&fs, home()).with_snapshots(
            data(),
            vec![
                snapshot("com.apple.TimeMachine.2026-09-20-101530.local", true),
                snapshot("com.apple.TimeMachine.2026-09-19-101530.local", false),
                snapshot("com.apple.os.update-abc", false),
            ],
        );

        let detected = Snapshots.detect(&world.context()).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(detected.findings.len(), 1, "{detected:?}");
        let finding = &detected.findings[0];
        assert_eq!(finding.id().to_string(), "snapshots.timemachine-local");
        assert_eq!(finding.reclaimable_bytes(), 0);
        assert_eq!(finding.item_count(), Some(1));
        assert_eq!(finding.reasoning(), Some("size not reported by macOS"));
        assert_eq!(finding.action(), Action::TmutilDelete);
        assert_eq!(finding.risk(), Risk::Amber);
        assert_eq!(finding.snapshots()[0].name, "com.apple.TimeMachine.2026-09-20-101530.local");
        assert_eq!(finding.snapshots()[0].volume, Some(data()));
        assert!(finding.snapshots()[0].mount_point.as_deref().is_some_and(|p| p.ends_with("Data")));
        assert!(finding.paths().is_empty());
    }

    #[test]
    fn a_purgeable_snapshot_without_a_uuid_is_reported_but_never_planned() {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        fs.add_dir(home());
        let anonymous =
            Snapshot { uuid: None, ..snapshot("com.apple.TimeMachine.2026-09-20-101530.local", true) };
        let world = context_over(&fs, home()).with_snapshots(data(), vec![anonymous]);

        let detected = Snapshots.detect(&world.context()).unwrap_or_else(|e| panic!("{e}"));

        assert!(detected.findings.is_empty(), "nothing a plan could name: {detected:?}");
    }

    #[test]
    fn a_machine_without_purgeable_snapshots_yields_no_finding() {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        fs.add_dir(home());
        let world = context_over(&fs, home())
            .with_snapshots(data(), vec![snapshot("com.apple.os.update-abc", false)]);

        let detected = Snapshots.detect(&world.context()).unwrap_or_else(|e| panic!("{e}"));

        assert!(detected.findings.is_empty(), "{detected:?}");
        assert!(detected.warnings.is_empty());
    }
}
