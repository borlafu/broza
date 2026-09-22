# Broza — Implementation Plan (Phase 1 CLI)

- Version: 1.0
- Date: 2026-09-21
- Status: Approved
- Inputs: [prd.md](prd.md) v1.0, [cli-spec.md](cli-spec.md) v1.1, [adr/](adr/)

## 1. Goals of Phase 1

Ship `broza` as an open-source (MIT) Rust CLI for Apple Silicon Macs on the two latest macOS
majors (26 and 27 today) that:

1. Enumerates disks, APFS containers, volumes with roles, and local snapshots (RF-01, RF-02).
2. Scans folders in parallel with a cache and explains what things are (RF-03, RF-04, RF-05, RF-06).
3. Suggests and executes cleanup under the safety invariants of PRD §7.3 (RF-07, RF-08, RF-09, RF-18, RF-19).
4. Exposes a stable JSON contract that the Phase 2 GUI will consume unchanged.

Deferred past v1.0: RF-10 (`launchd` scheduling), treemap rendering, native DiskArbitration adapter,
APFS clone-aware sizes, per-volume quarantine roots, native Spotlight (MDItem) bindings,
Docker daemon integration, FAT/exFAT.

## 2. Technology decisions

Research date: 2026-09-21. Versions are the latest observed on crates.io at that date.

| Concern | Choice | Notes |
|---|---|---|
| Workspace | `crates/broza` (core lib, crates.io `broza`) + `crates/broza-cli` (binary `broza`) | [ADR 0001](adr/0001-workspace-core-cli-split.md) |
| CLI parsing | `clap` 4 (derive) | `--help` snapshots with `insta` |
| Serialization | `serde`, `serde_json`, `toml`, `plist` 1.10 | JSON contract = `model/` types |
| Disk enumeration | `diskutil … -plist` via `ProcessRunner`, parsed with `plist` | [ADR 0002](adr/0002-diskutil-plist-over-diskarbitration.md) |
| Purgeable space | `objc2-foundation` 0.3, `NSURL` resource values | one `unsafe` adapter file |
| Directory walking | `dua-core` 4.x | `jwalk` archived 2026-08; fallback: `rayon` + `read_dir` (~150 lines) |
| Hard links | dedupe by `(dev, inode)` | pattern from `dust` / `dua-cli` |
| APFS clones | `getattrlist` `ATTR_CMNEXT_CLONEID` | best-effort, later; verify bit value on macOS 26/27 |
| Hashing | `blake3` | duplicates; optional quarantine integrity for files < 64 MB |
| Globs | `globset` | exclusions |
| Errors | `thiserror` (core), `anyhow` (binary) | one `BrozaError` → `ExitCode` |
| Tests | `tempfile`, `assert_cmd`, `predicates`, `insta`, `trybuild`, `cargo-llvm-cov` | coverage gate 80% |
| Release | `cargo-dist` 0.32 + `release-plz` | own Homebrew tap `borlafu/homebrew-broza` |
| SBOM | `cargo-cyclonedx` | RNF-05 |
| CI | GitHub Actions `macos-26` (arm64 GA) + `xcode-27` (macOS 27 preview) | add GA macOS 27 label when available |

Reuse candidates (port patterns, respect licenses): `kondo-lib` (project marker detection, MIT),
`mac-cleanup-py` (macOS path catalogue, Apache-2.0), `dust` / `dua-cli` (walker and inode dedupe,
Apache-2.0 / MIT), `duh` (APFS clone accounting, MIT).

## 3. Architecture

Planned layout (create as milestones require; keep files 200–400 lines):

```
Cargo.toml                         workspace, shared dependencies and lints
crates/broza/src/
  lib.rs
  model/      envelope.rs disk.rs finding.rs plan.rs quarantine.rs scan.rs ids.rs units.rs
  ports/      process.rs disk_enum.rs fs_ops.rs clock.rs prompter.rs mod.rs (Ports DI bundle)
  adapters/   diskutil/mod.rs (DiskutilEnumerator) budget.rs devices.rs inputs.rs parse.rs
              diskutil/{plist_list,plist_apfs,plist_info,plist_snapshots}.rs (pure parsers)
              diskutil/{roles,purpose}.rs (role predicate, plain-language text)
              diskutil/{assemble,volumes,hfs}.rs (parsed output → JSON contract)
              diskutil/snapshots.rs (DiskutilSnapshots) diskutil/tests_support.rs (cfg(test))
              nsurl_space.rs (purgeable, the only objc2 file) tmutil_destinations.rs
              mount_table.rs io_error.rs process_error.rs
              std_process.rs std_fs.rs system_clock.rs
  scan/       walker.rs aggregate.rs cache/{store,key}.rs mount.rs
  detect/     mod.rs filter.rs exclusions.rs explain.rs detectors/<category>.rs
  safety/     guard.rs policy.rs roles.rs path.rs exit_code.rs
  clean/      planner.rs executor.rs actions.rs
  quarantine/ store.rs manifest.rs mover.rs restore.rs expiry.rs
  config/     schema.rs layering.rs keys.rs
  error.rs
crates/broza/tests/fixtures/plist/<macos_major>/   recorded, redacted diskutil / tmutil output
crates/broza-cli/src/
  main.rs cli.rs args/<cmd>.rs commands/<cmd>.rs
  output/{mod,format,csv,color,bytes}.rs output/human/{scan,suggest,clean,restore,explain,quarantine}.rs
  tty_prompter.rs donate.rs env.rs
```

### 3.1 Domain model (JSON contract)

All types in `model/` derive `Serialize`, `Deserialize`, `Clone`, `Debug`, `PartialEq`, use
`#[serde(rename_all = "snake_case")]`, and enums are `#[non_exhaustive]`.

- `Envelope<T> { schema_version, broza_version, generated_at, command, host, data: T, warnings, errors }`
- `Disk → Container → Volume { role: VolumeRole, writable_by_broza, purpose, ... }`
- `Finding { id, category, title, description, risk, reclaimable_bytes, item_count, actionable, action, reasoning?, paths, instructions? }`
- `CleanPlan { dry_run, session_id, planned_bytes, quarantined_bytes, reclaimed_bytes, quarantine_path?, items }`
- `CleanItem { path, finding_id, size_bytes, status, action, error? }`
- `QuarantineSession { id, created_at, expires_at, total_bytes, state, entries }`

`Finding::new` enforces `category == cloud_synced ⇒ action == inform_only && !actionable`.

### 3.2 Safety kernel

See [ADR 0003](adr/0003-approved-token-safety-kernel.md). Order of checks in `guard::approve`:

1. `--apply` present, else the plan stays dry-run.
2. Canonicalize each path lexically plus `lstat`; reject symlinked components and relative paths.
3. Resolve the volume through a firmlink-aware mount table (`/Users/...` and
   `/System/Volumes/Data/Users/...` both resolve to the Data volume).
4. Reject roles `system`, `preboot`, `recovery`, `vm`. `backup` only for `tmutil_delete`.
5. Root allowlist: `$HOME`, `/Users/Shared`, `/private/var/folders/<uid dirs>`, `/Library/Caches`,
   `.Trashes` on data/user volumes, `/Applications` (only for `unused-apps`). Never the root itself.
6. Exclusions (config `exclude` + `--exclude`), then `--max-size`.
7. Any `inform_only` item rejects the whole plan.
8. `confirmation_policy(apply, max_risk, purge, yes, tty, ci)`:

| Condition (first match wins) | Mode |
|---|---|
| `!apply` | `None` (dry-run) |
| `max_risk == red` | `Rejected` → exit 2 |
| `purge && tty` | `TypedLiteral("PURGE")` (`--yes` ignored) |
| `purge && !tty` | `RequiredButNoTty` → exit 7 |
| `yes` | `None` |
| `!tty \|\| ci` | `RequiredButNoTty` → exit 7 |
| `green` | `SimpleYesNo` |
| `amber` | `DetailedExplicit` |

### 3.3 Quarantine store

See [ADR 0004](adr/0004-quarantine-same-volume-and-quarantine-command.md).
`~/.local/share/broza/quarantine/<cln_YYYYMMDDHHMMSS_xxxx>/manifest.json` + `items/<seq>/<basename>`.
Same-device rename only; cross-volume items are `skipped` with `cross_volume`.
Manifest written via tmp + rename after each item. Restore per session in reverse order.

### 3.4 Scan cache

`~/.cache/broza/v1/<volume_uuid>/dirs.bin`, versioned magic header, records keyed by
`(dev, inode, mtime_ns)` with aggregate bytes. Reuse a subtree when key matches and record age is
below `cache-ttl`. Any decode error → exit 9 with a `--no-cache` hint. Known imprecision: directory
mtime does not change on in-place file growth; bounded by TTL and documented.

### 3.5 Output

Core returns `Result<Envelope<T>, BrozaError>`. The CLI renders human text, JSON
(`serde_json` of the envelope, no custom code), or CSV (`scan`, `suggest`, `quarantine list`,
`restore --list`; otherwise exit 2). stdout carries data only. Color is disabled by
`--no-color`, `NO_COLOR`, `--json`, `--csv`, non-TTY stdout, or `CI`. Risk always carries a text
label (SAFE / REVIEW / INFO) in addition to color.

### 3.6 Testability seams

| Port | Fake | Fixture source |
|---|---|---|
| `ProcessRunner` | `FakeRunner` maps `(cmd, args)` → bytes | `tests/fixtures/plist/<macos_major>/*.plist` |
| `FileOps` | `FakeFileOps` in-memory tree with a `dev` per root | built in test |
| `Prompter` | `FakePrompter` scripted answers | — |
| `Clock` | `FixedClock` | — |
| `SpaceProvider`, `SnapshotProvider` | fakes returning fixed values | — |

A documented capture script records real `diskutil` / `tmutil` output and redacts UUIDs and serials.

## 4. Milestones

Each milestone: branch `m<N>-<slug>`, ends with tests green, coverage ≥ 80%, clippy clean, tag.

### M0 — Skeleton and contract

Scope: workspace; `model/*`; `Envelope`; `ExitCode`; `error.rs`; `units.rs` (size and duration
parsers per spec §3); clap tree for every command including `quarantine`, `about`, `config`
(get / set / list / path / reset with TOML layering); CI (`macos-26`, `xcode-27`); `cargo-dist`
init; LICENSE; README.

Exit criteria: `insta` snapshots for every JSON example in spec §4 round-trip; `--help` snapshots;
`broza about --json` is a valid envelope; exit codes 0 and 2 via `assert_cmd`. Nothing reads disks.

### M1 — Safety kernel

Scope: `safety/*` (`Approved<_>`, `confirmation_policy`, canonicalization, role check, root
allowlist, `--max-size`); `ports/*`; fakes; `clean` dry-run planner over synthetic findings.

Exit criteria: exhaustive truth-table test for the confirmation matrix; protected-role rejection
tests including firmlink paths; `trybuild` compile-fail test proving deletion needs `Approved`;
exit 6 and 7 paths tested end-to-end with `FakePrompter`.

### M2 — Read-only disk (release 0.1)

Scope: `ProcessRunner` with timeout; `diskutil` plist adapters with fixtures for macOS 26 and 27;
mount table; NSURL purgeable adapter; `dua-core` walker with hard-link dedupe; aggregate, top-N,
tree; cache store; `scan` (human / JSON / CSV); `explain` for volumes, paths, and categories.

Exit criteria: cold `scan` < 10 s and warm < 1 s on a 512 GB Data volume (benchmark script);
purgeable on its own line; fixture-driven enumeration tests; cache corruption → exit 9.
Homebrew tap publishes 0.1.

Progress:

- [x] `ProcessRunner` with a hard timeout and its own process group (`adapters/std_process.rs`).
- [x] `diskutil` plist adapters with macOS 26 fixtures (`adapters/diskutil/`).
- [ ] **macOS 27 fixtures — deferred**: no machine running 27 is available to record them
      (`scripts/capture-diskutil-fixtures.sh` is the recorder). The parsers are written against the
      documented plist keys and tolerate unknown ones, so the gap is in *evidence*, not in support.
      Record them on the first 27 machine that appears and add them under
      `crates/broza/tests/fixtures/plist/macos27/`; the fixture runner finds a directory by name and
      needs no code change. This is the one open item of the M2 exit criteria.
- [x] Mount table, firmlink-aware (`scan/mount.rs`, `adapters/mount_table.rs`).
- [x] NSURL purgeable adapter (`adapters/nsurl_space.rs`).
- [x] `scan`: human, `--json`, `--csv` (the volume table); `--volume` by id, name or mount point;
      `--no-external`; purgeable always on its own line and labelled an estimate; container-level
      percentages and usage bar.
- [x] `explain` for volumes, paths and categories, with `--short` and `--json` (spec §4.7).
- [x] CLI wiring of the real adapters (`broza-cli/src/wiring.rs`), TTY prompter, and the host
      block behind the `ProcessRunner` port.
- [x] Debug-only `BROZA_FAKE_DISKUTIL_FIXTURES` seam so the binary is testable end to end.
- [x] Parallel walker (`rayon` + `getattrlistbulk`; `dua-core` rejected because it cannot run
      on the `FileOps` fakes) with deterministic hard-link dedupe; aggregate, top-N, tree
      (`scan/walker/`, `scan/aggregate.rs`).
- [x] Cache store, TTL, corruption → exit `9` (`scan/cache/`).
- [x] Walker wired into `broza scan`: `largest_items`, `--tree`, `PATH` arguments, progress on stderr.
- [x] Benchmark script for the cold and warm `scan` targets (`scripts/bench-scan.sh`, ADR 0006).
- [ ] APFS clone accounting (post-1.0): a whole-volume walk that exceeds `used_bytes` warns with
      `size_exceeds_volume` until then.
- [ ] Homebrew tap publishing 0.1.

M3 progress:

- [x] `Detector` trait, `DetectContext` over the shared home walk, `Registry` (failures →
      `detector_failed` warnings), risk/size filters (`detect/`).
- [x] `user-cache` (`library-caches`, `logs`, `incomplete-downloads`) and `build-cache`
      (`xcode-deriveddata`, `xcode-archives`, `orphan-node-modules`, `pycache`, `gradle-caches`,
      `cargo-target`, `docker-raw` inform-only) detectors.
- [x] `broza suggest`: human (risk groups, text labels), `--json`, `--csv`, `--category`, `--risk`,
      `--min-size`, `--unused-after`, `--explain`; one home walk that refreshes `scan`'s cache.
- [x] Review round on detectors + `suggest`: totals count actionable findings only
      (`inform_only_bytes`), overlapping paths credited once, orphan `node_modules` rule narrowed
      (tool-managed trees excluded, idleness from the project's entries), JetBrains `LocalHistory`
      kept out of the green caches, an unreadable `~/Downloads` costs one finding not the category
      (`location_unreadable`), registry tests, walk nodes moved not cloned.
- [ ] Warm `suggest` (post-0.2): cache records that know whether a subtree holds any name a detector
      looks for, so the cache may answer for the rest. Today `suggest` walks cold by design
      (`docs/cli-spec.md` §7).
- [x] `broza clean`: detection shared with `suggest` (`commands/detection.rs`), planner → guard →
      confirmation (`TtyPrompter`) → pre-execution expiry (`commands/clean_expiry.rs`) → mover →
      report; `--max-size`, `--exclude`, `--risk`/`--category` mandatory; inform-only findings inside
      an actionable category are skipped with a warning; the store is created on first `--apply`.
- [ ] `clean --apply --purge` execution (needs the irreversible path in the mover; M4 with `trash`).
- [x] `restore` (`ID…`, `--session`, `--all`, `--to`, `--list` with `--csv`) and
      `quarantine list | expire | purge` (`commands/{restore,quarantine,store}.rs`); every write inside
      the store through `Approved<QuarantineWrite>` from the manifests' own paths; `purge` asks the
      typed word after refusing unknown ids.
- [x] End-to-end in process: `clean --apply -y` → `quarantine list` → `restore --session` puts the
      bytes back unchanged and empties the store (`commands/quarantine_tests.rs`).
- [x] Donation gate (RF-17): `Outcome::reclaimed` marks an applied cleanup, `donate_display`
      gathers the six conditions, prints the two lines on stderr and rewrites the marker file
      (`~/.local/share/broza/state/donate_last_shown`); a marker that cannot be written is a `-v` note.
- [x] Release 0.2: version 0.2.0, `CHANGELOG.md`, `dist-workspace.toml` (Apple Silicon only,
      shell + Homebrew installers, tap `borlafu/homebrew-broza`), `.github/workflows/release.yml`.
      Tagged `v0.2.0` locally; publish when a remote exists.

### M3 — Green detectors and quarantine (release 0.2)

Scope: `Detector` trait, `Registry`, filters, exclusions; detectors `user-cache` and `build-cache`
(DerivedData, Archives, orphan `node_modules`, `__pycache__`, `.gradle`, `target/`, `Docker.raw`
inform-only); `quarantine/*`; `clean --apply`; `quarantine list | expire | purge`; `restore`;
donation gate (RF-17, six conditions, 30-day marker); `suggest`.

Exit criteria: end-to-end `assert_cmd` in a tempdir: dry-run → `--apply -y` → `restore --session`
restores byte-identical content; cross-volume skip test; donation table test; dry-run reports
`quarantined_bytes = reclaimed_bytes = 0` and all items `planned`.

Status: the round trip runs in process against the fake machine (`commands/quarantine_tests.rs`),
not through `assert_cmd`: the recorded fixture's filesystem is in memory and does not survive from
one process to the next, and an `assert_cmd` run against the real disk would break the rule that
no test touches `$HOME`. The cross-volume skip is covered in `crates/broza/tests/quarantine_roundtrip.rs`
and its exit-5 reporting in `commands/clean_tests.rs`; the donation table in `donate.rs`.

### M4 — Amber detectors

Scope: `trash` (purge action), `snapshots` (`diskutil apfs listSnapshots -plist`,
`tmutil deletelocalsnapshots`; no size available; never `com.apple.os.update-*`), `old-backups`
(`MobileSync/Backup`, parse `Info.plist`), `ios-simulators` (`xcrun simctl list -j` with timeout),
`duplicates` (size → 4 KiB prefix → full `blake3`), `large-old-files` (`max(atime, kMDItemLastUsedDate)`).

Exit criteria: each detector has fixture-tree tests plus one negative test; full `suggest` < 15 s
on the development machine.

Design decisions taken at the start of M4 (2026-09-22), in the order the work lands:

1. **Trash detector** (`detect/detectors/trash.rs`): `~/.Trash` from the home walk plus
   `<mount>/.Trashes/<uid>` on every writable, unprotected volume, read directly (one `read_dir`
   per volume; a trash Broza may not read is a `location_unreadable` warning). Paths are the direct
   children; action `purge` is the category default, risk amber. No safety change: `.Trashes` is
   already a conditional allowed root.
2. **Irreversible executor** (`clean/executor.rs`; the mover keeps moving only `quarantine` items):
   `Action::Quarantine` items move into the session as today; `Action::Purge` items are re-checked against the token's
   `(device, inode)` and removed with `remove_tree`, recorded `purged`, their allocated bytes added to
   `reclaimed_bytes`; `Action::TmutilDelete` items go to `SnapshotProvider::delete`, which takes an
   `Approved<SnapshotDelete>` (new port method; `diskutil apfs deleteSnapshot <volume> -uuid <uuid>`
   in the adapter — one snapshot on one volume, [ADR 0007](adr/0007-snapshot-deletion-by-uuid.md);
   `permission_denied` → item `failed` and the exact command in a warning). A plan without quarantine
   items creates no session, so `quarantine_path` is absent. This lifts the "not implemented"
   refusal of `clean --apply --purge`.
3. **Snapshot items in a plan**: `CleanItem` gains an optional `snapshot` object
   (`{ volume, name }`, schema 1.1 additive while unreleased); its `path` is the volume's mount point
   and is informational. The guard checks such an item without touching the filesystem: the finding
   is of the `snapshots` category, the name and UUID are ones the finding listed with `purgeable: true`
   and the `com.apple.TimeMachine.` prefix, the volume is mounted where the item says with a data or
   user role, and `size_bytes` is `0`. The planner builds these
   items from `finding.snapshots()`.
4. **Snapshots detector**: `SnapshotProvider::list` on every data/user APFS volume; only purgeable
   `com.apple.TimeMachine.*` snapshots are listed; `reclaimable_bytes: 0` with the reasoning
   "size not reported by macOS".
5. **Old backups**: `~/Library/Application Support/MobileSync/Backup/<udid>/Info.plist` read through
   `FileOps` and parsed with `plist` (`Device Name`, `Product Name`, `Last Backup Date`); a backup
   whose last date is older than `unused-after` is proposed, one path per backup, sized from the walk.
6. **iOS simulators**: `xcrun simctl list -j devices,runtimes` through `ProcessRunner` with a 10 s
   timeout and a recorded fixture; unavailable devices and devices not booted for `unused-after`
   are proposed as `~/Library/Developer/CoreSimulator/Devices/<udid>`; a failing or missing `xcrun`
   is a warning, never an abort.
7. **Large old files**: files of at least 1 GB reported by the walk, `last_used =
   max(atime, kMDItemLastUsedDate)` with `mdls` through `ProcessRunner` for the candidates only,
   low-confidence reasoning when Spotlight has no value; older than `unused-after` is proposed.
8. **Duplicates**: files of at least 1 MB reported by the walk, grouped by size, then by the first
   4 KiB (`FileOps::read_prefix`, new), then by full BLAKE3 (`FileOps::hash_file`, new, adapter-side);
   the oldest copy is kept, the rest proposed; `.git` object stores are skipped.

The home walk of `suggest` gains a file report (`ScanRequest.files_min_size`, decoupled from
`min_size`) for items 7 and 8; the cache rule is unchanged.

M4 progress:

- [x] 1. `trash` detector (`detect/detectors/trash.rs`).
- [x] 2. Irreversible executor for `purge` (`clean/executor.rs`; the mover moves only `quarantine`
      items); `clean --apply --purge` enabled. `tmutil_delete` waits for step 3.
- [x] 3. Snapshot items in the plan and the guard (`CleanItem.snapshot`, `check_snapshot_item`,
      `snapshot_deletions`, `SnapshotProvider::delete`, `tmutil deletelocalsnapshots`).
- [x] 4. `snapshots` detector (`detect/detectors/snapshots.rs`).
- [x] 5. `old-backups` detector (`detect/detectors/old_backups.rs`).
- [x] 6. `ios-simulators` detector (`detect/detectors/ios_simulators.rs`; recorded
      `simctl_devices.json` beside the plist recordings).
- [x] 7. `large-old-files` detector (`detect/detectors/large_old_files.rs`; the home walk reports
      files of at least 1 GB through `ScanRequest.file_report`).
- [ ] 8. `duplicates` detector.
- [ ] `quarantined_bytes` reports the guard-verified *apparent* size for files while every other
      figure is allocated (`quarantine/attempt.rs::measured_size`); pin the unit in §4.4 and
      switch to allocated, or record the deviation in an ADR.
- [ ] `suggest` under 15 s on the development machine; review; release 0.3.

### M5 — Inform-only, apps, polish (release 1.0)

Scope: `cloud-synced` (iCloud Drive, Dropbox, OneDrive, Google Drive; evicted / dataless
detection; provider instructions); `unused-apps` (configurable threshold, `~/Library` leftovers by
bundle id); profiles (`developer`); Full Disk Access warning path (exit 3 only when the operation
is impossible); SBOM in releases; `--locked` reproducible build; README complete; Ko-fi link in `about`.

Exit criteria: `clean --apply` on any `cloud-synced` finding → exit 2; traceability table (§6)
fully covered by tests; coverage ≥ 80%; tag `v1.0.0`.

## 5. Testing strategy

- TDD for every module: failing test, implementation, refactor.
- Unit tests beside code; integration tests per crate; `assert_cmd` for the binary.
- Fixtures per macOS major under `crates/broza/tests/fixtures/`.
- Snapshot tests (`insta`) for JSON examples and `--help`.
- No test touches real disks or spawns system tools; all macOS behavior goes through fakes.
- Coverage gate 80% lines via `cargo llvm-cov` in CI.
- Benchmarks (`scan`, `suggest`) as a script, not a CI gate; results recorded per release.

### 5.1 Debug-only test seams of the binary

Two environment variables let `assert_cmd` drive the real `broza` binary without a real Mac
underneath it. Both are read only in **debug** builds; a release binary ignores them entirely, so
neither can be used to talk a shipped Broza out of looking at the actual system.

| Variable | Effect | Gate |
|---|---|---|
| `BROZA_HOST=<macos_version>/<arch>` | Pins the `host` block of the envelope instead of asking `sw_vers`. | `#[cfg(debug_assertions)]` |
| `BROZA_FAKE_DISKUTIL_FIXTURES=<dir>` | Replaces the `ProcessRunner` with one replaying the recorded `diskutil` / `tmutil` plists in `<dir>`, and pins the purgeable estimate (which comes from Foundation, not from a command). | `#[cfg(debug_assertions)]` **and** the `broza-cli` feature `fake-diskutil` |

`<dir>` is a directory such as `crates/broza/tests/fixtures/plist/macos26`. The mapping is by file
name: `list.plist`, `apfs_list.plist`, `tmutil_destinationinfo.plist`, and one `info_*.plist` per
device, filed under the `DeviceIdentifier` the recording itself declares
(`broza::testing::fixture_runner`).

## 6. Requirements traceability

| Requirement | Milestone |
|---|---|
| RF-01 enumeration | M2 |
| RF-02 real usage, hard links once | M2 (hard links); APFS clones deferred |
| RF-03 hierarchical scan + cache | M2 |
| RF-04 plain-language roles | M2 |
| RF-05 tree + usage bars (treemap deferred) | M2 |
| RF-06 human, `--json`, `--csv` | M0 (contract), M2 (scan), M3 (suggest / clean) |
| RF-07 dry-run default | M1 |
| RF-08 quarantine, TTL, `--purge` | M3 |
| RF-09 protected volumes, exclusions | M1 (roles), M3 (exclusions) |
| RF-10 scheduled cleanups | Deferred post-1.0 |
| RF-17 donation message | M3 |
| RF-18 explicit confirmation, exit 7 | M1 |
| RF-19 cloud-synced inform-only | M1 (guard), M5 (detector) |
| RF-11 … RF-16 | Phase 2 |
| RNF-01 data safety | M1, M3 |
| RNF-02 performance | M2, M4 |
| RNF-03 privacy (no telemetry) | M0 (no network code), all |
| RNF-04 compatibility | M0 (CI matrix), M2 (fixtures) |
| RNF-05 signed binaries, reproducible build, SBOM | M5 (SBOM, `--locked`); notarization when a Developer ID exists |
| RNF-06 accessibility (GUI) | Phase 2; text risk labels in CLI from M3 |
| RNF-07 English CLI | M0 |

## 7. Open technical questions

Carried into milestone work; each becomes a fixture-backed test, not an assumption.

1. `dua-core` 4.x: confirm per-entry `dev` / `ino` and subtree skipping for cache hits.
2. `statfs.f_mntonname` behavior for firmlinked paths on macOS 26 / 27.
3. `ATTR_CMNEXT_CLONEID` bit value on macOS 26 / 27 (`0x100` observed vs `0x40` documented).
4. Whether `tmutil deletelocalsnapshots` needs root on macOS 26 / 27.
5. `xcrun simctl list -j` needs Xcode command-line tools; can be slow on first run. Enforce a timeout and warn when it fails.
6. Accuracy of the NSURL purgeable estimate versus Disk Utility; label as an estimate.
