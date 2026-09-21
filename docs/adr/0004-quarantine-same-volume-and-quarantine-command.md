# ADR 0004 — Quarantine is a same-volume move; `broza quarantine` frees space

- Status: Accepted
- Date: 2026-09-21
- Changes: CLI spec 1.0 → 1.1 (new command, new `clean` JSON fields)

## Context

Moving a 90 GB directory into `~/.local/share/broza/quarantine/` on the same APFS volume is an
atomic rename that frees nothing until the item is purged. Copying across volumes would double
space usage temporarily and still free nothing. The draft spec had no command to purge or expire
quarantine, and its `reclaimed_bytes` implied immediate gain.

## Decision

- Quarantine store: `~/.local/share/broza/quarantine/<session_id>/manifest.json` plus
  `items/<seq>/<basename>`. Manifest written via tmp + rename; states `in_progress`,
  `complete`, `restoring`.
- Items are moved only when source and quarantine root share the same device id. Otherwise the
  item is `skipped` with error `cross_volume` and a hint (set `quarantine-path` on that volume,
  or use `--purge`). Per-volume quarantine roots are a later extension.
- New command `broza quarantine list | expire | purge`. `purge` is irreversible: typed literal
  `PURGE`, ignores `--yes`. `expire` removes sessions past `quarantine-ttl` (default 30 days).
- `clean --apply` expires sessions past TTL before executing, under the same confirmation as green risk.
- `clean` JSON reports `quarantined_bytes` (pending, reversible) and `reclaimed_bytes`
  (actually freed by purge or `tmutil`). Dry-run reports 0 for both.
- Restore is per session, reverse order; a path collision skips that entry unless `--to` is given.

## Consequences

- Users see truthful numbers and an explicit way to free space.
- External-disk cleanup needs `--purge` or a quarantine path on that disk in v1.
- PRD KPI "GB freed" becomes "GB made reclaimable (quarantined + reclaimed)".
