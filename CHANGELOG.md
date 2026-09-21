# Changelog

All notable changes to Broza are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow semver.
The JSON contract has its own version (`schema_version`, `docs/cli-spec.md` §4.1).

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
