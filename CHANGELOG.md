# Changelog

All notable changes to Broza are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow semver.
The JSON contract has its own version (`schema_version`, `docs/cli-spec.md` §4.1).

## [1.0.0] — 2026-09-22

The last two categories, the safety kernel closing the cloud-root gap, and the supply-chain
pieces a 1.0 needs.

### Added

- `cloud-synced`: one inform-only finding per provider present (iCloud Drive, Dropbox, OneDrive,
  Google Drive, other File Provider roots) with the local bytes the provider could release and
  its official steps. Broza never deletes a synced file.
- `unused-apps`: applications not opened for `--unused-after` by Spotlight's record (amber),
  applications Spotlight has no record of (red, inform only, with steps to check them by hand),
  and leftovers of uninstalled applications under `~/Library` caches, logs, HTTP storage and saved
  state, matched by bundle identifier (amber).
- A CycloneDX SBOM in every release; the PRD traceability table names the tests behind each
  requirement.

### Changed

- The safety kernel refuses any plan item under a cloud-provider root, whatever detector named it
  and however the path is spelled (`InsideCloudRoot`, exit `2`). The five roots are excluded from
  the home walk as well.
- `scan <PATH>` exits `3` when macOS refuses to list the named path itself; a refusal inside stays
  a warning, and the hint names the setting to change. `suggest` and `clean` keep going.
- `large-old-files` and `unused-apps` share one Spotlight (`mdls`) helper.

## [0.3.0] — 2026-09-22

The amber detectors, irreversible deletion behind a typed `PURGE`, and a scan cache that lets
`suggest` run warm.

### Added

- The amber detectors: `trash` (`~/.Trash` and `.Trashes` on other volumes; `purge` action),
  `snapshots` (purgeable Time Machine local snapshots, deleted by UUID on their own volume through
  `diskutil apfs deleteSnapshot`; size reported as unknown, never summed), `old-backups` (iOS
  device backups older than `--unused-after`), `ios-simulators` (devices with a gone runtime or not
  booted for `--unused-after`), `large-old-files` (files of 1 GB or more neither opened nor changed
  for `--unused-after`, Spotlight's last-used date through `mdls`), `duplicates` (identical files of
  1 MB to 1 GB in the user's own trees, confirmed by BLAKE3; the oldest copy kept).
- `clean --apply --purge`: irreversible deletion after typing `PURGE`; `--max-size` covers purges.
- Warnings `snapshot_needs_admin`, `spotlight_unavailable`, `file_report_truncated`.

### Changed

- Scan cache layout 3: records carry their child directories and their files from 1 MB, so an
  unchanged subtree is served whole; `suggest` runs warm (under 9 s on a developer's home that
  walks cold in 36 s) and `clean` walks cold on purpose. A store from an older Broza is
  replaced, not refused (ADR 0008).
- `--min-size` keeps findings whose size macOS does not report (snapshots).
- `quarantined_bytes` is allocated bytes, like `reclaimed_bytes` and `reclaimable_bytes`.
- Snapshot deletion uses `diskutil apfs deleteSnapshot <volume> -uuid <uuid>` instead of
  `tmutil deletelocalsnapshots <date>`, which is machine-wide (ADR 0007).

## [0.2.0] — 2026-09-21

First release that writes to the disk, always through the quarantine store.

### Added

- `broza suggest`: the `user-cache` and `build-cache` detectors over one walk of the home;
  risk-grouped human output, `--json`, `--csv`, `--category`, `--risk`, `--min-size`,
  `--unused-after`, `--explain`. Totals count only what Broza can act on; inform-only
  findings are reported apart (`inform_only_bytes`).
- `broza clean`: dry run by default; `--apply` moves items into a quarantine session after the
  confirmation the risk level requires (`y/N` for green, detailed for amber, exit `7` without a
  terminal and without `--yes`); expires past-due sessions first; `--max-size`, `--exclude`.
- `broza restore` (`ID…`, `--session`, `--all`, `--to`, `--list`) and
  `broza quarantine list | expire | purge` (`purge` requires typing `PURGE`; `--yes` is refused).
- Donation message on stderr after a successful applied cleanup, under the six conditions of
  the specification (§5), at most once every 30 days.
- Detection warnings: `detector_failed`, `location_unreadable`; clean warnings:
  `inform_only_skipped`, `expiry_pending`, `expiry_declined`, `expiry_unreadable`.

### Not yet

- `clean --apply --purge` exits `1` ("not implemented") until the `trash` detector lands (M4).
- The amber detectors (trash, snapshots, old backups, simulators, duplicates, large old files)
  and the inform-only `cloud-synced` category.

## [0.1.0] — 2026-09-21

Read-only release: `broza scan` (disks, containers, volumes, purgeable space, largest
consumers, `--tree`), `broza explain` (volumes, paths, categories), `broza config`,
`broza about`. Never published to Homebrew; superseded by 0.2.0 the same day.
