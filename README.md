# Broza — safe disk cleanup CLI for macOS

Broza is an open-source command-line tool that analyzes your Mac's APFS storage, explains what
"System Data" and purgeable space really are, and frees disk space with a dry-run first and a
reversible quarantine. Built in Rust for Apple Silicon.

> Status: pre-release. Phase 1 (CLI) is in active development. See the [roadmap](#roadmap).

<!-- Badges (activated at first release)
[![CI](https://github.com/borlafu/broza/actions/workflows/ci.yml/badge.svg)](https://github.com/borlafu/broza/actions)
[![crates.io](https://img.shields.io/crates/v/broza-cli.svg)](https://crates.io/crates/broza-cli)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
-->

## Why Broza

Every Mac owner eventually asks the same three questions when the disk fills up:

- **What do I have?** Broza maps physical disks, APFS containers, volumes and their roles, and local snapshots.
- **What is each thing for?** Broza explains every volume and folder in plain English.
- **What can I delete without breaking anything?** Broza suggests cleanup by risk level and only
  acts after you confirm, moving items to a quarantine you can undo.

The name is Spanish: *broza* means brushwood or useless clutter. Pronounced BRO-zah.

## Quick start

Install with Homebrew (available from release 0.1):

```bash
brew install borlafu/broza/broza
```

Or with Cargo:

```bash
cargo install broza-cli --locked
```

See what is using your disk:

```bash
broza scan
```

Get cleanup suggestions grouped by risk:

```bash
broza suggest
```

Preview a safe cleanup. This is a dry-run; nothing is deleted:

```bash
broza clean --risk green
```

Apply it. Items move to quarantine, and Broza asks for confirmation first:

```bash
broza clean --risk green --apply
```

Example `scan` output:

```
Physical disk  disk0  —  APPLE SSD AP1024Z  (1.00 TB)
└─ APFS container  disk3  (994.66 GB)
   ├─ Used          812.40 GB  ████████████████░░░░  81.7%
   ├─ Free          98.12 GB
   └─ Purgeable     84.14 GB   ← macOS shows this as "available"

   Volumes in this container:
   ┌ Macintosh HD            System    11.30 GB   read-only, sealed
   ├ Macintosh HD - Data     Data     798.21 GB   ← your data
   ├ Preboot                 Preboot    6.52 GB
   ├ Recovery                Recovery   1.20 GB
   └ VM                      VM         3.00 GB   swap
```

## Why it is safe

Broza is designed so that a mistake cannot cost you data.

- **Dry-run by default.** `broza clean` only simulates. Nothing changes without `--apply`.
- **Reversible by default.** Deleted items go to a quarantine for 30 days. `broza restore` puts them back.
- **Irreversible deletion is deliberate.** `--purge` requires typing the word `PURGE`. `--yes` does not skip it.
- **System volumes are untouchable.** Broza never writes to the System, Preboot, Recovery, or VM
  volumes. There is no flag to change that.
- **Cloud files are only reported.** Files already synced to iCloud Drive, Dropbox, OneDrive, or
  Google Drive are never deleted. Broza shows the provider's official steps instead.
- **Honest numbers.** Purgeable space is shown separately from free space. Quarantined bytes are
  reported separately from bytes actually freed.
- **Scriptable.** Every command has `--json`. No prompt appears without a terminal.

## What it cleans

| Category | What it finds | Risk | Action |
|---|---|---|---|
| `user-cache` | `~/Library/Caches`, logs, incomplete downloads | Safe | quarantine |
| `build-cache` | Xcode DerivedData and Archives, orphan `node_modules`, `__pycache__`, `.gradle`, `target/`, Docker disk image | Safe / Review | quarantine |
| `ios-simulators` | Unused iOS simulator runtimes and devices | Review | quarantine |
| `trash` | Trash on every volume | Review | empty |
| `snapshots` | APFS local snapshots (Time Machine) | Review | `tmutil` |
| `old-backups` | Old iPhone and iPad backups in `MobileSync/Backup` | Review | quarantine |
| `unused-apps` | Apps not opened in over a year, plus their leftovers | Review | quarantine |
| `cloud-synced` | Files already stored in iCloud Drive, Dropbox, OneDrive, Google Drive | Info only | report |
| `duplicates` | Identical files by content hash | Review | quarantine |
| `large-old-files` | Large files not opened in a long time | Review | quarantine |

Full details, flags, and the JSON contract are in the [CLI specification](docs/cli-spec.md).

## Understanding your Mac's storage

**APFS containers share space.** A Mac's SSD holds one APFS container with several volumes
(System, Data, Preboot, Recovery, VM). Free space belongs to the container, not to any single
volume. That is why a "percentage full" per volume is misleading, and why Broza reasons at the
container level.

**Free is not the same as available.** Finder reports "available" as free space plus
*purgeable* space. Purgeable space is data macOS may delete on its own when it needs room, such as
local snapshots and caches. An app asking the file system sees only the truly free part. Broza
shows both numbers on separate lines and never adds purgeable to free.

**What "System Data" contains.** The large, opaque "System Data" bucket in System Settings is
mostly APFS local snapshots, virtual memory swap, application caches, logs, and on newer Macs
the Apple Intelligence models. Some of it is safe to remove, some shrinks after a reboot, and
some should only be managed through Apple's own tools. `broza explain` tells you which is which.

**Snapshots need `tmutil`.** APFS local snapshots are copy-on-write images made by Time Machine
and by system updates. They can take tens of gigabytes and must be managed with `tmutil`, never
by deleting files by hand. Broza uses `tmutil` for this and never touches update snapshots.

## Requirements

- macOS 26 or 27 (Broza supports the two latest major versions).
- Apple Silicon (M-series). Intel Macs are not supported.
- No special permissions to enumerate disks or scan your home folder.
- Full Disk Access is optional. It lets Broza scan protected areas such as Mail, Safari, and
  device backups. Without it, Broza scans what it can and tells you what it skipped.

## Scripting and JSON

Every command supports `--json`. Output is a stable envelope:

```json
{
  "schema_version": "1.1",
  "broza_version": "0.1.0",
  "generated_at": "2026-09-21T10:36:08Z",
  "command": "suggest",
  "host": { "macos_version": "26.1", "arch": "arm64" },
  "data": {},
  "warnings": [],
  "errors": []
}
```

Sizes are integers in bytes. Timestamps are ISO 8601 UTC. Consumers should ignore unknown fields.

Exit codes:

| Code | Meaning |
|---|---|
| 0 | Success, including "nothing to clean" |
| 1 | Unclassified error |
| 2 | Invalid arguments or flag combination |
| 3 | Permission denied (for example, missing Full Disk Access) |
| 4 | Volume, path, or quarantine item not found |
| 5 | Partial failure; see `errors[]` |
| 6 | Aborted by the user |
| 7 | Confirmation required but no terminal and no `--yes` |
| 8 | Unsupported macOS version or architecture |
| 9 | Cache corrupted; retry with `--no-cache` |

Setting `CI` disables prompts, progress, and the support message. `NO_COLOR` disables color.

## Roadmap

Phase 1 delivers the CLI in six milestones:

1. **M0** Skeleton, JSON contract, command tree, CI.
2. **M1** Safety kernel: dry-run, confirmation policy, protected volumes.
3. **M2** Read-only analysis: `scan` and `explain`. First release (0.1).
4. **M3** Safe detectors, quarantine, `clean`, `restore`. Release 0.2.
5. **M4** Review-level detectors: trash, snapshots, backups, simulators, duplicates, large files.
6. **M5** Cloud-synced reporting, unused apps, profiles, release 1.0.

Phase 2 is a paid desktop GUI built on the same open-source core. Details in the
[implementation plan](docs/implementation-plan.md) and the [product requirements](docs/prd.md).

## Contributing

Contributions are welcome once the M0 skeleton lands. The project follows test-driven
development, conventional commits, and a set of safety invariants that no pull request may weaken.
Human contributors: read the [PRD](docs/prd.md) and [CLI spec](docs/cli-spec.md). AI coding
agents: read [AGENTS.md](AGENTS.md).

## Support the project

Broza is free and will stay free. If it saved you space, you can leave a tip at
[ko-fi.com/broza](https://ko-fi.com/broza). It is a donation, nothing is sold and nothing is unlocked.

## License

The Broza core and CLI are released under the [MIT License](LICENSE). The planned Phase 2
desktop app will be proprietary, built on this same MIT core.

## FAQ

**Does Broza delete files permanently?**
Not by default. Items go to a quarantine for 30 days and can be restored with `broza restore`.
Permanent deletion requires `--purge` and typing `PURGE`.

**Why does Finder show more available space than Broza?**
Finder adds purgeable space to free space. Broza shows them separately because purgeable space
is not guaranteed to be available to your apps right now.

**Can Broza remove Time Machine local snapshots?**
Yes, through `tmutil`, and only after you confirm. macOS does not report per-snapshot sizes, so
Broza lists them without a size estimate. Update snapshots are never proposed.

**Is it safe to delete Xcode DerivedData?**
Yes. DerivedData contains build artifacts that Xcode regenerates on the next build. Broza marks
it as a safe, green-risk item.

**Does it work on Intel Macs?**
No. Broza targets Apple Silicon only.

**Does Broza send any data anywhere?**
No. Broza has no telemetry and makes no network requests.
