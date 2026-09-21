# ADR 0002 — Disk enumeration via `diskutil -plist` behind a trait (v1)

- Status: Accepted
- Date: 2026-09-21
- Supersedes: PRD draft v0.5 RF-01 wording ("via DiskArbitration")

## Context

RF-01 requires enumerating disks, APFS containers, volumes with roles, partitions, and snapshots.
Research on 2026-09-21 showed:

- `objc2-disk-arbitration` 0.3.2 exists and is maintained, but exposes BSD names, volume
  paths, and device models only. It does not expose APFS roles, container free space, or purgeable bytes.
- `diskutil apfs list -plist` exposes `Roles[]`, `CapacityFree`, `CapacityInUse`, physical stores,
  encryption state. `diskutil info -plist` adds mount point, `Sealed`, `Internal`, snapshot flags.
  `diskutil apfs listSnapshots -plist` lists snapshots with a `Purgeable` flag (no size).
- No `diskutil` output contains purgeable space; Foundation's
  `NSURLVolumeAvailableCapacityForImportantUsageKey` does.

## Decision

- v1 implements `DiskEnumerator` by running `diskutil` with `-plist` through a `ProcessRunner`
  port (timeout enforced) and parsing with the `plist` crate.
- Purgeable bytes come from a small `objc2-foundation` adapter:
  `purgeable = max(0, important_usage_available - available)`, labeled as an estimate.
- Snapshots come from `diskutil apfs listSnapshots -plist` and `tmutil listlocalsnapshots`.
- A native DiskArbitration adapter may be added later behind the same trait, without changing callers.

## Consequences

- Fully testable with recorded plist fixtures per macOS major; no `unsafe` in enumeration.
- Dependency on `diskutil` output schema: parse with `#[serde(default)]`, map unknown roles to
  `unknown` (never writable), keep fixtures for each supported macOS version.
- One process spawn per `diskutil` call adds tens of milliseconds; acceptable within the
  `scan` budget.
