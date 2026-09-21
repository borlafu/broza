# Broza — CLI Specification

**Specification version:** 1.1
**Status:** Approved for implementation
**Scope:** Phase 1 (open source CLI, MIT)
**Platform:** macOS 26 and 27 (the two latest major versions), Apple Silicon only
**Date:** 2026-09-21

> This document supersedes draft 1.0 (`broza-cli-spec.md`, Spanish). All changes relative to 1.0 are listed in §9 "Changelog 1.0 → 1.1". Requirement identifiers (`RF-xx`, `RNF-xx`) refer to the Broza PRD. This specification is normative: "MUST", "MUST NOT", "SHOULD" and "MAY" are used in their RFC 2119 sense.

---

## 0. Design principles

These principles are binding. Whenever an implementation question arises, it MUST be resolved in their favour.

1. **Nothing is deleted without explicit confirmation.** `clean` is a dry run by default. Writing to disk requires `--apply`.
2. **Every deletion is reversible.** By default items are moved to quarantine, not destroyed. Irreversible deletion requires `--purge` plus an additional confirmation.
3. **Explain before acting.** The user must be able to understand *what* each item is and *why* its removal is proposed.
4. **First-class scriptability.** Every human-readable output has a `--json` equivalent. Nothing interactive is triggered without a TTY.
5. **stdout is for data, stderr is for conversation.** Warnings, progress, prompts and the donation message always go to stderr.
6. **Honest numbers.** Never present *purgeable* space as if it were immediately free space.
7. **Never touch system volumes.** `System`, `Preboot`, `Recovery` and `VM` are read-only for Broza, with no exception and no flag that allows otherwise.
8. **Honest about quarantine.** Moving an item to quarantine does not free space until the quarantine session expires or is purged. The CLI always shows pending bytes (quarantined) and freed bytes (reclaimed) separately.

---

## 1. General invocation

```
broza [COMMAND] [ARGUMENTS] [OPTIONS]
```

Without a command, `broza` prints a short help summary and exits with code `0`.

### 1.1 Global flags

Available on every command unless stated otherwise.

| Flag | Alias | Type | Default | Description |
|---|---|---|---|---|
| `--json` | | bool | `false` | JSON output to stdout. Disables color, progress and interactivity. |
| `--csv` | | bool | `false` | Flat CSV output to stdout. Only on tabular commands: `scan`, `suggest`, `quarantine list`, `restore --list`. Any other command exits `2`. |
| `--output <f>` | `-o` | path | — | Write output to a file instead of stdout. |
| `--no-color` | | bool | auto | Disables ANSI color. Automatic when there is no TTY or `NO_COLOR` is set. |
| `--quiet` | `-q` | bool | `false` | Errors only. Silences progress and non-critical warnings. |
| `--verbose` | `-v` | count | `0` | Repeatable (`-vv`, `-vvv`) for more diagnostic detail on stderr. |
| `--config <f>` | | path | `~/.config/broza/config.toml` | Alternative configuration file. |
| `--profile <n>` | | string | `default` | Configuration profile to apply (e.g. `developer`). |
| `--no-cache` | | bool | `false` | Ignore the scan cache and force a full analysis. |
| `--version` | `-V` | bool | | Broza version and JSON schema version. |
| `--help` | `-h` | bool | | Help for the command. |

`--json` and `--csv` are mutually exclusive; combining them exits `2`.

### 1.2 Environment variables

| Variable | Effect |
|---|---|
| `NO_COLOR` | If set (any value), disables color. |
| `BROZA_CONFIG` | Path to the configuration file. Lower priority than `--config`. |
| `BROZA_NO_DONATE` | If set, silences the donation message. |
| `CI` | If set, a non-interactive environment is assumed: no prompts, no progress, no donation message. |

### 1.3 Configuration precedence

From lowest to highest priority:

```
built-in defaults  <  config.toml  <  profile (--profile)  <  environment variables  <  command-line flags
```

### 1.4 Value grammars

**Duration** — `<int><unit>`, where `unit` is one of:

| Unit | Meaning |
|---|---|
| `h` | hours |
| `d` | days |
| `w` | weeks (7 days) |
| `m` | months (30 days) |
| `y` | years (365 days) |

Examples: `24h`, `30d`, `6m`, `1y`. Minutes are not supported; `m` always means months. A duration without a unit is a usage error (exit `2`).

**Size** — `<number><unit>`, where `number` is a non-negative decimal (`1.5` allowed) and `unit` is one of:

| Unit family | Units | Base |
|---|---|---|
| Decimal (SI) | `B`, `KB`, `MB`, `GB`, `TB` | 10^3 (`1 GB` = 1,000,000,000 bytes) |
| Binary (IEC) | `KiB`, `MiB`, `GiB`, `TiB` | 2^10 (`1 GiB` = 1,073,741,824 bytes) |

Units are case-insensitive on input; whitespace between number and unit is optional. Both families are accepted on input. **All human-readable output uses decimal units with a decimal point** (`138.2 GB`, `1.00 TB`), matching Finder and Disk Utility. JSON output always carries integer bytes (see §4.1).

---

## 2. Exit codes

A single code space for the whole CLI. Scripts MUST be able to distinguish "I did nothing" from "I failed".

| Code | Name | Meaning |
|---|---|---|
| `0` | `OK` | Success. Includes "nothing found to clean" and "nothing to expire". |
| `1` | `GENERIC_ERROR` | Unclassified error. |
| `2` | `USAGE_ERROR` | Invalid arguments, flags or flag combinations. |
| `3` | `PERMISSION_DENIED` | Missing *Full Disk Access* or insufficient permissions on a path. |
| `4` | `TARGET_NOT_FOUND` | Volume, path, category, session or quarantine item does not exist. |
| `5` | `PARTIAL_FAILURE` | The operation finished, but some items failed or were skipped. See `errors[]` in the JSON. |
| `6` | `ABORTED_BY_USER` | The user answered "no" to a confirmation prompt. |
| `7` | `CONFIRMATION_REQUIRED` | Confirmation was required and no TTY was available (and `--yes` was not passed or is not honoured). |
| `8` | `UNSUPPORTED_SYSTEM` | Unsupported macOS version or architecture. |
| `9` | `CACHE_ERROR` | Scan cache corrupt or unreadable. Retry with `--no-cache`. |

**Explicit cases:**

| Situation | Exit code |
|---|---|
| `clean` dry run (no `--apply`) with a correct analysis, whatever the planned size | `0` |
| `clean --apply` where the selection contains any red or `inform_only` item (e.g. `cloud-synced`) | `2` — the whole plan is rejected before anything is executed |
| `--yes` together with `--purge` | `2` — `--purge` never accepts an implicit "yes" |
| `--csv` on a command that does not support it | `2` |
| `--json` together with `--csv` | `2` |
| Cross-volume quarantine (item on a different device than the quarantine root) | Item `skipped` with `error: "cross_volume"`; overall exit `5` if other items succeeded, `0` only if the plan was empty |
| `--purge` or `quarantine purge` without a TTY | `7` |
| `quarantine expire` with nothing past TTL | `0` |
| Safety-kernel refusal (protected volume role, symlink component, path outside the allowed roots, exclusion, `--max-size` exceeded, relative or `..` path, inconsistent plan) | `2` — the request is invalid; nothing is executed |
| An item's path no longer exists at `--apply` time | Item `skipped` with `error: "not_found"`; the plan continues; exit `5` if other items succeeded |
| OS denies reading or acting on a path (`EACCES`/`EPERM`) | Item `failed` with `error: "permission_denied"`; whole-command `3` only when the operation was impossible without the permission |
| `clean` dry run whose selection contains only `inform_only` findings | `0` with a warning; the plan lists nothing and stderr points to `broza explain <category>` |

> **Rule:** `clean` in dry-run mode always returns `0` if the analysis succeeded, even if it proposes removing 200 GB. A dry run is not a failure.

---

## 3. Commands

### 3.1 `broza scan`

Analyses storage and presents the system map: physical disks, APFS containers, volumes and real usage.

```
broza scan [PATH...] [OPTIONS]
```

| Option | Type | Default | Description |
|---|---|---|---|
| `--depth <n>` | int | `2` | Depth of the folder tree shown. |
| `--top <n>` | int | `20` | Number of largest items to list. |
| `--min-size <s>` | size | `100MB` | Ignore items below this size. Accepts `500MB`, `2GB`, `1GiB`. |
| `--volume <id>` | string | all | Restrict the scan to one volume (device id, name or mount point). A disk or container identifier selects that whole level. Blank → exit `2`; names nothing → exit `4`. |
| `--no-external` | bool | `false` | Exclude mounted external disks. By default externals are included. Applied before `--volume`, so a volume on an excluded disk is then "not found". |
| `--tree` | bool | `false` | Force the hierarchical tree view. |

Supports `--json` and `--csv`. The CSV output is the **volume table**, one row per volume, with the header:

```
disk_id,container_id,volume_id,name,role,mount_point,used_bytes,writable_by_broza
```

`role` is a value of the `role` enum of §4.1, `used_bytes` an integer, `writable_by_broza` `true` or `false`, and `mount_point` empty for a volume macOS does not mount. Fields are quoted only when their content requires it (RFC 4180).

> `scan --csv` reports volumes and not `largest_items`. The two tables live at different levels — one row per volume against one row per path — and mixing them in one file would give a consumer no stable column set. `largest_items` is reachable through `--json`; if a flat form of it is ever needed it gets its own command or flag, not this one.

**Human output (sketch):**

```
Physical disk  disk0  —  APPLE SSD AP1024Z  (1.00 TB)
└─ APFS container  disk3  (994.7 GB)
   ├─ Used        812.4 GB  ████████████████░░░░  81.7%
   ├─ Free         98.1 GB
   └─ Purgeable    84.1 GB  ← estimate; macOS shows this as "available"

   Volumes in this container:
   ┌ Macintosh HD         System     11.3 GB   read-only, sealed
   ├ Macintosh HD - Data  Data      798.2 GB   ← your data
   ├ Preboot              Preboot     6.5 GB
   ├ Recovery             Recovery    1.2 GB
   └ VM                   VM          3.0 GB   swap

Largest consumers on Macintosh HD - Data:
  312.4 GB  ~/Library/Developer            (Xcode)
   84.1 GB  ~/Library/Caches
   61.7 GB  ~/Documents
```

A disk macOS does not report as internal is marked `(external)` after its size; a partition
carrying `HFS+` is announced as `HFS+ volume` rather than as an APFS container. A volume row
carries `not writable` when its role would allow writing but the volume does not (`data` and
`user` volumes only), and `swap`, `Time Machine` or `read-only, sealed` for the roles that have
a standing explanation.

> **Mandatory UX rule:** purgeable space is **always** shown on its own line and is never added to free space without a label. Usage percentages are computed at container level, never per volume (APFS volumes share space).

---

### 3.2 `broza explain`

Explains in plain language what a volume, path or cleanup category is and what it is for. This command embodies principle 3: *understand* before cleaning.

```
broza explain <PATH|VOLUME|CATEGORY> [OPTIONS]
```

| Option | Type | Default | Description |
|---|---|---|---|
| `--short` | bool | `false` | One-line summary only. |

The target is resolved in this order: exact category id (§3.3) → volume (device id, name or mount point) → filesystem path. Unknown targets exit `4`. A path target is made absolute against the working directory and cleaned of `.` and `..` **lexically** — nothing is followed and `..` never climbs above `/` — and it must exist: a path that does not exist is an unknown target, not an excuse to describe the working directory's volume. The explanation of a path is the explanation of the volume it lives on, plus one line naming that volume. The JSON form is §4.7; `--csv` exits `2`.

Examples of valid targets: `broza explain /System/Volumes/Data`, `broza explain disk3s1`, `broza explain "Macintosh HD"`, `broza explain cloud-synced`, `broza explain snapshots`.

**Example (volume):**

```
$ broza explain /System/Volumes/Data

Macintosh HD - Data   ·   APFS role: Data   ·   798.2 GB

What it is:
  The mutable data volume of macOS. It holds /Users, the applications you
  install, your user libraries and caches. It is joined to the system volume
  through "firmlinks", which is why Finder shows a single disk.

What it is for:
  This is where practically everything that belongs to you lives. Around 99%
  of what Broza can help you clean is here.

Is it safe to touch?
  Yes, with judgement. It is not a system volume. Broza never modifies the
  System, Preboot, Recovery or VM volumes.
```

**Example (category):**

```
$ broza explain snapshots --short

snapshots  ·  REVIEW  ·  APFS local snapshots (Time Machine, OS update). Managed only via tmutil; sizes not reported by macOS.
```

---

### 3.3 `broza suggest`

The core of the product. Detects cleanable categories, estimates reclaimable space and assigns a risk level. **Never writes anything.**

```
broza suggest [OPTIONS]
```

| Option | Type | Default | Description |
|---|---|---|---|
| `--category <c>` | string[] | all | Restrict to specific categories. Repeatable or comma-separated. |
| `--risk <level>` | enum | `all` | `green`, `amber`, `red`, `all`. Filter by risk. |
| `--min-size <s>` | size | `50MB` | Omit smaller findings. |
| `--unused-after <p>` | duration | `1y` | "Unused app" threshold. Accepts `6m`, `1y`, `2y`. |
| `--explain` | bool | `false` | Include the reasoning behind each detection. |

Supports `--json` and `--csv`. The CSV output is the flattened `findings` table (`id,category,title,risk,action,actionable,reclaimable_bytes,item_count`).

**Category identifiers** (stable, usable in scripts):

| ID | Description | Base risk | Action |
|---|---|---|---|
| `user-cache` | `~/Library/Caches`, logs, incomplete downloads | green | `quarantine` |
| `build-cache` | DerivedData, Archives, orphan `node_modules`, `__pycache__`, `.gradle`, `target/`; `Docker.raw` as inform-only sub-finding | green / amber | `quarantine` (Docker.raw: `inform_only`) |
| `ios-simulators` | Unused iOS simulator runtimes and devices | amber | `quarantine` |
| `trash` | Trash folders on all volumes | amber | `purge` |
| `snapshots` | APFS local snapshots (Time Machine) | amber | `tmutil_delete` |
| `old-backups` | iOS device backups in `MobileSync/Backup` | amber | `quarantine` |
| `unused-apps` | Apps not opened past the threshold, plus their leftovers | amber / red | `quarantine` |
| `cloud-synced` | Files already synced to a cloud provider | red — **inform only** | `inform_only` |
| `duplicates` | Identical copies by content hash | amber | `quarantine` |
| `large-old-files` | Large files not opened for a long time | amber | `quarantine` |

**Category notes (normative):**

- `snapshots`: macOS does not expose per-snapshot sizes through any public interface. The finding MUST report `reclaimable_bytes: 0` with `reasoning` "size not reported by macOS", MUST list each snapshot name with its `purgeable` flag, and MUST NOT propose `com.apple.os.update-*` snapshots (they are not purgeable and are required for the pending update). Only `com.apple.TimeMachine.*` snapshots flagged purgeable are actionable.
- `build-cache`: the Docker virtual disk (`Docker.raw`, or the `.raw` file under `~/Library/Containers/com.docker.docker/`) is reported as an inform-only sub-finding with its **allocated** size on disk. Broza never calls the Docker daemon; the `instructions` block points to `docker system prune` and Docker Desktop's disk settings.
- `build-cache` / orphan `node_modules`: a `node_modules` directory is orphan when its parent has no `package.json`, or when the parent directory's mtime is older than `unused-after`. In monorepos only leaf `node_modules` are proposed.
- `duplicates`: candidates are grouped by size, then by the first 4 KiB, then confirmed by a full BLAKE3 hash. Only confirmed groups are reported.
- `large-old-files` / `unused-apps`: `last_used` is `max(atime, kMDItemLastUsedDate)`. When Spotlight returns no value (typical for system apps) the finding's `reasoning` MUST state low confidence.
- `cloud-synced`: always `risk: red`, `actionable: false`, `action: inform_only`. No flag changes this.

**Human output (sketch):**

```
Potentially reclaimable space:  246.8 GB

SAFE (green) — 138.2 GB
   build-cache      121.4 GB   Xcode DerivedData (94.2 GB), orphan node_modules (27.2 GB)
                               Docker.raw: 38.6 GB allocated — inform only, see: broza explain build-cache
   user-cache        16.8 GB   Regenerable application caches

REVIEW (amber) — 96.1 GB
   snapshots           0 B     4 Time Machine local snapshots (size not reported by macOS)
   unused-apps       28.7 GB   6 apps not opened in more than 1 year
   old-backups       15.1 GB   2 iPhone backups from 2023
   duplicates        52.3 GB   318 duplicate groups

INFO ONLY (red) — 12.5 GB
   cloud-synced      12.5 GB   Already backed up in iCloud Drive and Dropbox
                               → Broza does not delete this. See: broza explain cloud-synced

Next step:
  broza clean --risk green            (dry run, deletes nothing)
  broza clean --risk green --apply    (moves to quarantine; space is freed after expiry or purge)
```

Risk is always rendered with a text label (`SAFE`, `REVIEW`, `INFO ONLY`) in addition to color (RNF-06).

---

### 3.4 `broza clean`

Executes the cleanup. **Dry run by default.**

```
broza clean [OPTIONS]
```

| Option | Type | Default | Description |
|---|---|---|---|
| `--apply` | bool | `false` | **Required to modify the disk.** Without it, only simulates. |
| `--category <c>` | string[] | — | Categories to clean. Mandatory unless `--risk` is used. |
| `--risk <level>` | enum | — | Clean everything at this level or lower. `green` is the recommended usage. |
| `--purge` | bool | `false` | Irreversible deletion, bypassing quarantine. Requires an additional typed confirmation. |
| `--yes` | `-y` | bool | `false` | Assume "yes" on confirmations. **Not allowed together with `--purge`** (exit `2`). |
| `--max-size <s>` | size | — | Safety cap: abort (exit `2`) if more than this amount would be removed. |
| `--exclude <p>` | glob[] | — | Patterns to exclude. Repeatable. Merged with `exclude` from config. |
| `--unused-after <p>` | duration | `1y` | Threshold for `unused-apps`. |

Supports `--json`. Does not support `--csv` (exit `2`).

**Confirmation matrix:**

| Situation | Behaviour |
|---|---|
| Without `--apply` | Simulates, reports and exits `0`. Never prompts. |
| `--apply` with green risk only | Summary + simple confirmation (`y/N`). |
| `--apply` including amber risk | Detailed summary (every path) + explicit confirmation (`y/N`). |
| `--apply` including red / `inform_only` | **Rejected**, exit `2`. `cloud-synced` cannot be removed by Broza. |
| `--purge` | Double confirmation. The literal word `PURGE` must be typed. Ignores `--yes`; combining them exits `2`. |
| Plan contains a natively irreversible action (`trash` → `purge`, `snapshots` → `tmutil_delete`) without `--purge` | Detailed summary flagged **irreversible** + explicit confirmation (`y/N`). `--yes` is honoured: these categories are amber and were selected explicitly. |
| `--yes` (no `--purge`) | Skips the y/N prompt. Also covers the automatic expiry step below. |
| No TTY and no `--yes` | Exits `7` (`CONFIRMATION_REQUIRED`). Does not act. |
| `CI` set | Treated as no TTY. |

**Automatic expiry on `--apply`:** before executing the plan, Broza expires every quarantine session whose `expires_at` is in the past (same effect as `broza quarantine expire`). This step uses the same confirmation level as green (`y/N`, covered by `--yes`); it is reported in `data.expired_sessions` and its freed bytes count towards `reclaimed_bytes`. A dry run lists sessions that *would* expire but does not touch them.

**Cross-volume rule:** the default quarantine root lives on the Data volume. An item that resides on a different device than the quarantine root (an external disk, a second APFS container) cannot be moved by rename; copying it would take time and free nothing. Such items are marked `skipped` with `error: "cross_volume"` and a hint on stderr: set `quarantine-path` to a directory on that volume (`broza config set quarantine-path /Volumes/External/.broza-quarantine`), or use `--purge` for that category. The rest of the plan proceeds; the exit code is `5` if anything else succeeded.

**Execution order:** validate flags → build plan → safety checks (below) → confirmation → expire past-TTL sessions → execute items in plan order → write manifest → report.

**Safety checks (all MUST pass before confirmation is even requested):**

1. `--apply` is present.
2. Each path is canonicalised without following symlinks and resolved to its volume through the mount table, firmlink-aware (`/Users/...` and `/System/Volumes/Data/Users/...` resolve to the same Data volume).
3. The volume role is not `system`, `preboot`, `recovery` or `vm`.
4. The path is under an allowed root: `$HOME`, `/Users/Shared`, `/private/var/folders/<uid>`, `/Library/Caches`, `.Trashes` on data or user volumes, `/Applications` (only for `unused-apps`).
5. The path does not match any exclusion.
6. Total planned bytes do not exceed `--max-size`. The cap is applied **twice**, on two different figures, and the two are not interchangeable:
   - *Before execution*, against the scanned sizes: a plan whose total already exceeds the cap is refused outright and the command exits `2`. File sizes have been re-measured by the safety kernel at this point; directory sizes are the scan's aggregate.
   - *During execution*, against the measured size of each directory: the executor re-measures a directory immediately before moving it (allocated bytes, not apparent bytes), and an item whose real size would take the running total past the cap is left in place and marked `skipped` with `error: "max_size_exceeded"`. The rest of the plan proceeds.
   A directory is only re-measured when `--max-size` is given: without a cap the scanned figure is the one reported, and walking the tree again would cost time that changes no decision.
7. No item in the plan is `inform_only`.

> **Safety invariant:** no combination of flags allows Broza to write to a volume with role `System`, `Preboot`, `Recovery` or `VM`. There is no override flag, and there will not be one. Any request to add such a flag MUST be rejected.

---

### 3.5 `broza restore`

Recovers items from quarantine.

```
broza restore [ID...] [OPTIONS]
```

| Option | Type | Default | Description |
|---|---|---|---|
| `--list` | bool | `false` | List quarantine contents without restoring. Supports `--json` and `--csv`. |
| `--all` | bool | `false` | Restore everything currently in quarantine. |
| `--session <id>` | string | — | Restore one complete cleanup session. |
| `--to <path>` | path | original | Restore to an alternative location instead of the original path. |

`ID` is either a session id (`cln_…`) or an item id (`cln_…/<seq>`). At least one of `ID...`, `--all` or `--session` is required (else exit `2`). Unknown ids exit `4`.

Quarantine lives in `~/.local/share/broza/quarantine/` (configurable via `quarantine-path`), one directory per cleanup session, with a configurable TTL (default **30 days**). Every cleanup session receives an identifier of the form `cln_YYYYMMDDHHMMSS_xxxx`. Each session directory holds a `manifest.json` (written atomically via temp file + rename; `state` is `in_progress`, `complete` or `restoring`) and an `items/<seq>/<basename>` tree.

**Concurrency:** every operation that writes to a session holds an advisory lock on
`<session>/.lock` for its whole duration. `restore`, `quarantine expire` and `quarantine purge`
take it without waiting: a session another Broza is working on is reported `session_busy` and
left untouched, never half-restored and never removed. `quarantine list` never waits and never
creates a lock file; it reports a busy session with the `session_busy` warning. An unknown
session id is still exit `4`, refused before anything is written; a session that becomes
unreadable *during* a multi-session run is reported in `errors[]` and the run continues (exit `5`).

**Restore semantics:**

- Restoration is **per session**: items are restored in reverse sequence order. Each item that goes back leaves the manifest immediately, so a restore is never all-or-nothing on disk; an item that cannot go back is reported (`skipped` or `failed`) and **stays in quarantine**, and the session keeps `state: restoring` so the whole command can simply be run again. A session is removed only once its `items/` directory is empty — never because its manifest lists nothing, which is the corruption case and is reported as `session_has_untracked_items` instead.
- **Collision rule:** if the original path already exists, the entry is `skipped` with `error: "collision"` unless `--to` is given, in which case the item is placed under `--to` preserving its basename. Broza never overwrites an existing path.
- Restored items are removed from the session manifest; an empty session is deleted.
- Exit `5` if some entries were skipped or failed, `0` if all succeeded.

---

### 3.6 `broza config`

```
broza config get <key>
broza config set <key> <value>
broza config list [--json]
broza config path
broza config reset [<key>]
```

`set` validates the value against the key's type before writing (invalid → exit `2`). `reset` without a key restores all defaults after a `y/N` confirmation (no TTY and no `--yes` → `7`).

**Recognised keys:**

| Key | Type | Default | Description |
|---|---|---|---|
| `unused-after` | duration | `1y` | Unused-app threshold. |
| `quarantine-ttl` | duration | `30d` | Retention period in quarantine. |
| `quarantine-path` | path | `~/.local/share/broza/quarantine` | Quarantine location. Must be on the same device as the items it will hold (§3.4). |
| `min-size` | size | `50MB` | Default minimum size for findings. |
| `donate-prompt` | bool | `true` | Ko-fi support message (RF-17, §5). |
| `color` | enum | `auto` | `auto`, `always`, `never`. |
| `exclude` | glob[] | `[]` | Permanent exclusions. |
| `cache-ttl` | duration | `24h` | Validity of the scan cache. |

**Example `config.toml`:**

```toml
# Global defaults. Values here are overridden by --profile, environment variables and flags.
unused-after   = "1y"
quarantine-ttl = "30d"
donate-prompt  = true
exclude = [
  "~/Projects/**/node_modules",          # never touch node_modules inside active projects
  "~/Library/Caches/com.mycompany.*",    # company tooling caches
]

# Profile for day-to-day development machines: broza suggest --profile developer
[profiles.developer]
min-size   = "500MB"
categories = ["build-cache", "ios-simulators", "duplicates"]
```

---

### 3.7 `broza about`

Prints version, license, JSON schema version, platform support and the support link. All output goes to stdout; it never shows the donation prompt (it *is* the place for the link). Supports `--json`.

```
$ broza about
Broza 0.1.0  ·  MIT License  ·  JSON schema 1.1
Safe disk cleanup for macOS 26/27 on Apple Silicon.
Source:   https://github.com/borlafu/broza
Support:  https://ko-fi.com/broza  (donation, nothing in return)
```

---

### 3.8 `broza quarantine`

Manages the quarantine store. **This is how space is actually freed:** `clean --apply` only moves items into quarantine (principle 8); bytes are reclaimed when a session expires or is purged.

```
broza quarantine list   [--json|--csv]
broza quarantine expire [--yes]
broza quarantine purge  [<SESSION_ID>...|--all]
```

#### 3.8.1 `quarantine list`

Lists quarantine sessions. Supports `--json` and `--csv`. Columns / fields per session: `id`, `created_at`, `expires_at`, `total_bytes`, `item_count`, `state` (`in_progress` · `complete` · `restoring` · `expired`).

```
$ broza quarantine list
Session                    Created              Expires              Size       Items  State
cln_20260917103608_a1b2    2026-09-17 10:36     2026-10-17 10:36     138.2 GB   1284   complete
cln_20260801091200_c3d4    2026-08-01 09:12     2026-08-31 09:12      12.4 GB     37   expired

Pending in quarantine:  150.6 GB   (12.4 GB past TTL — run: broza quarantine expire)
```

#### 3.8.2 `quarantine expire`

Permanently deletes every session whose `expires_at` is in the past. Asks a simple `y/N` confirmation showing the sessions and total bytes; `--yes` skips it. No TTY and no `--yes` → `7`. If nothing is past TTL, prints "Nothing to expire." and exits `0`. Supports `--json` (same shape as §4.6 with `operation: "expire"`).

#### 3.8.3 `quarantine purge`

Permanently deletes the given sessions (or all of them with `--all`) regardless of TTL. **Irreversible.** Requires typing the literal word `PURGE`; `--yes` is ignored (and combining `--yes` with `purge` exits `2`). Without a TTY → `7`. Unknown session ids → `4`. Sessions in state `restoring` are refused (exit `5` with `error: "session_busy"`). Supports `--json` (§4.6 with `operation: "purge"`).

```
$ broza quarantine purge cln_20260917103608_a1b2
This will permanently delete 1 session (138.2 GB, 1,284 items). This cannot be undone.
Type PURGE to continue: PURGE
Freed 138.2 GB.
```

---

## 4. JSON contract

> **This is the most important artefact in the specification.** It is the contract between the Rust core and the future SwiftUI GUI. Changing it later is expensive; designing it well now means Phase 2 rewrites nothing.

### 4.1 Common envelope

Every `--json` output shares this structure:

```json
{
  "schema_version": "1.1",
  "broza_version": "0.1.0",
  "generated_at": "2026-09-21T10:36:08Z",
  "command": "suggest",
  "host": {
    "macos_version": "26.1",
    "arch": "arm64"
  },
  "data": {},
  "warnings": [],
  "errors": []
}
```

**Schema rules:**

- `schema_version` follows *semver*. A **minor** bump only adds optional fields; a **major** bump may remove or rename fields.
- All sizes are **integers in bytes**, never formatted strings. Formatting belongs to the presentation layer.
- All timestamps are **ISO 8601 / RFC 3339 in UTC**.
- Consumers MUST **ignore unknown fields** and **unknown enum values** without failing.
- Optional fields are **absent**, never `null`, when they have no value.
- Values of the enums persisted in the quarantine manifest (`status`, `state`, `error`) and of
  `type` MUST survive a read-modify-write cycle **verbatim**: a reader that does not know a value
  keeps the original token instead of rewriting it. `role` is the exception: an unrecognised role
  is read as `unknown`, because the role decides write protection (§6) and the safest reading of an
  unknown role is "not writable".
- Field names are `snake_case`; identifiers (categories, finding ids) are `kebab-case` with `.` as the detector separator.
- `errors[]` entries have the shape `{ "code": "<stable_code>", "message": "<human text>", "path": "<optional>" }`. A non-empty `errors[]` implies exit code `5` when the operation was partial.
- `warnings[]` entries have the same shape; they never affect the exit code.

**Envelope codes of the quarantine store** (stable; consumers ignore codes they do not know):

| Code | Where | Meaning |
|---|---|---|
| `manifest_corrupt` | `errors[]` | A session's `manifest.json` cannot be read. The session is skipped and never removed; the rest of the store is still listed or processed. |
| `orphaned_item` | `errors[]` on `expire`, `warnings[]` on `purge` | The session holds an item its manifest does not list. `expire` refuses such a session; `purge` removes it and says so. |
| `untracked_bytes` | `warnings[]` | `quarantine list`: the session holds more than its manifest accounts for, and its `total_bytes` includes it. |
| `session_incomplete` | `warnings[]` | The session is not marked finished: a `clean` may still be running. |
| `session_busy` | `errors[]`, or `warnings[]` on `list` | Another Broza holds the session's lock. Nothing is moved or removed; `list` still shows it. |
| `session_left_behind` | `warnings[]` | Every item was restored but the empty session directory could not be removed. |
| `session_has_untracked_items` | `errors[]` | A restore emptied the manifest while the directory still holds files. The session is kept in `restoring` and reported; this is the corruption case. |
| `exclusive_rename_unsupported` | `warnings[]` | The filesystem has no atomic exclusive rename (exFAT, some network volumes), so the destination was checked first. Nothing was replaced, but the move was not atomic. |

**Stable enums** (consumers ignore unknown values; producers MUST NOT rename existing values without a major bump):

| Enum | Values |
|---|---|
| `role` | `system` · `data` · `preboot` · `recovery` · `vm` · `backup` · `user` · `unknown` |
| `type` (container filesystem) | `apfs` · `hfs_plus` · any other token, passed through unchanged |
| `risk` | `green` · `amber` · `red` |
| `action` | `quarantine` · `purge` · `tmutil_delete` · `inform_only` |
| `status` (clean / restore item) | `planned` · `quarantined` · `purged` · `restored` · `skipped` · `failed` · `moving` (manifest only: written before the rename and replaced after it, so an interrupted move is detectable; reading a session settles it and no command ever reports it) |
| `state` (quarantine session) | `in_progress` · `complete` · `restoring` · `expired` |
| `category` | `user-cache` · `build-cache` · `ios-simulators` · `trash` · `snapshots` · `old-backups` · `unused-apps` · `cloud-synced` · `duplicates` · `large-old-files` |
| `error` (item-level code) | `cross_volume` · `permission_denied` · `collision` · `not_found` · `protected_volume` · `session_busy` · `io_error` · `changed_since_check` (the item's `(device, inode)` changed between the safety check and the write; nothing was touched) · `max_size_exceeded` (the re-measured size would exceed `--max-size`; item left in place) |

### 4.2 `scan`

```json
{
  "data": {
    "disks": [{
      "id": "disk0",
      "model": "APPLE SSD AP1024Z",
      "size_bytes": 1000555581440,
      "internal": true,
      "containers": [{
        "id": "disk3",
        "type": "apfs",
        "size_bytes": 994662584320,
        "used_bytes": 812400000000,
        "free_bytes": 98120000000,
        "purgeable_bytes": 84140000000,
        "volumes": [{
          "id": "disk3s1",
          "name": "Macintosh HD",
          "role": "system",
          "mount_point": "/",
          "used_bytes": 11300000000,
          "writable_by_broza": false,
          "purpose": "Read-only, sealed and signed operating system volume (SSV)."
        }]
      }]
    }],
    "largest_items": [{
      "path": "/Users/x/Library/Developer",
      "size_bytes": 312400000000,
      "kind": "directory",
      "volume_id": "disk3s5"
    }]
  }
}
```

`purgeable_bytes` is an estimate derived from Foundation's `NSURLVolumeAvailableCapacityForImportantUsageKey` minus `NSURLVolumeAvailableCapacityKey` for the same volume, clamped at 0 (both values come from one Foundation call, so they are mutually consistent; `free_bytes` comes from `diskutil` and may differ by a few MB); the human output labels it as an estimate. Hard links are counted once in `size_bytes`; APFS clones are counted once where detectable (best-effort, completed post-1.0, PRD RF-02). Cloud placeholders whose contents are not on the disk (`SF_DATALESS`: iCloud Drive, Files On-Demand) contribute 0 bytes and are never listed in `largest_items`.

`volumes[].mount_point` is **optional**: volumes that macOS does not mount, typically `Preboot`
and `Recovery`, are enumerated with their role and size but without a mount point, and the field
is then absent.

`volumes[].uuid` is **optional** too: the filesystem UUID (`APFSVolumeUUID`, or `VolumeUUID` for
`HFS+`), absent when macOS does not report one. It is what the scan cache of §7 is filed under,
because a BSD name like `disk3s5` belongs to whatever is plugged in today while the UUID follows
the volume.

### 4.3 `suggest`

```json
{
  "data": {
    "total_reclaimable_bytes": 246800000000,
    "by_risk": {
      "green": 138200000000,
      "amber": 96100000000,
      "red": 12500000000
    },
    "findings": [{
      "id": "build-cache.xcode-deriveddata",
      "category": "build-cache",
      "title": "Xcode DerivedData",
      "description": "Xcode build artefacts. They are regenerated automatically.",
      "risk": "green",
      "reclaimable_bytes": 94200000000,
      "item_count": 1284,
      "actionable": true,
      "action": "quarantine",
      "reasoning": "Not opened in 94 days. Xcode regenerates this directory on the next build.",
      "paths": [{
        "path": "/Users/x/Library/Developer/Xcode/DerivedData",
        "size_bytes": 94200000000,
        "last_used": "2026-06-15T09:12:00Z"
      }]
    }, {
      "id": "snapshots.timemachine-local",
      "category": "snapshots",
      "title": "Time Machine local snapshots",
      "description": "APFS copy-on-write snapshots kept locally by Time Machine.",
      "risk": "amber",
      "reclaimable_bytes": 0,
      "item_count": 4,
      "actionable": true,
      "action": "tmutil_delete",
      "reasoning": "size not reported by macOS",
      "snapshots": [{
        "name": "com.apple.TimeMachine.2026-09-20-101530.local",
        "purgeable": true
      }]
    }, {
      "id": "cloud-synced.icloud",
      "category": "cloud-synced",
      "title": "Already backed up in iCloud Drive",
      "risk": "red",
      "reclaimable_bytes": 12500000000,
      "actionable": false,
      "action": "inform_only",
      "instructions": {
        "provider": "iCloud Drive",
        "summary": "Use Apple's official feature to release local copies.",
        "steps": [
          "System Settings → [your name] → iCloud → iCloud Drive",
          "Turn on \"Optimize Mac Storage\""
        ]
      }
    }]
  }
}
```

**Key fields:**

| Field | Contract |
|---|---|
| `id` | Stable identifier `category.detector`. Usable as a key by the GUI. |
| `risk` | `green` · `amber` · `red`. |
| `actionable` | If `false`, Broza **cannot** remove it. The GUI MUST disable the control. |
| `action` | `quarantine` · `purge` · `tmutil_delete` · `inform_only`. |
| `instructions` | Present **exactly** when `action` is `inform_only`, and required there: a finding Broza refuses to act on MUST tell the user what to do instead. Contains the provider's official steps. |
| `snapshots` | Present only for `category: snapshots`. Each entry has `name`, `purgeable` and an optional `uuid` (present when macOS reports one). |
| `paths[].last_used` | Optional. Absent when neither `atime` nor Spotlight provides a value. |

### 4.4 `clean`

```json
{
  "data": {
    "dry_run": false,
    "session_id": "cln_20260921103608_a1b2",
    "planned_bytes": 111000000000,
    "quarantined_bytes": 94200000000,
    "reclaimed_bytes": 12400000000,
    "quarantine_path": "/Users/x/.local/share/broza/quarantine/cln_20260921103608_a1b2",
    "expired_sessions": [{
      "id": "cln_20260801091200_c3d4",
      "freed_bytes": 12400000000
    }],
    "items": [{
      "path": "/Users/x/Library/Developer/Xcode/DerivedData",
      "finding_id": "build-cache.xcode-deriveddata",
      "size_bytes": 94200000000,
      "status": "quarantined",
      "action": "quarantine"
    }, {
      "path": "/Volumes/External/Projects/old/node_modules",
      "finding_id": "build-cache.orphan-node-modules",
      "size_bytes": 16800000000,
      "status": "skipped",
      "action": "quarantine",
      "error": "cross_volume"
    }]
  }
}
```

**Byte counters (normative definitions):**

| Field | Meaning |
|---|---|
| `planned_bytes` | Sum of `size_bytes` of every item in the plan, regardless of outcome. |
| `quarantined_bytes` | Bytes moved into quarantine in this run. **Pending**: still occupying disk until expiry or purge. |
| `reclaimed_bytes` | Bytes actually freed in this run: `purge` items, `tmutil_delete` items, and sessions expired in the pre-execution step. |

| Mode | `quarantined_bytes` | `reclaimed_bytes` | Item `status` |
|---|---|---|---|
| Dry run (no `--apply`) | `0` | `0` | all `planned` |
| `--apply`, default (quarantine) | increases | unchanged by the plan itself (only expiry contributes) | `quarantined` / `skipped` / `failed` |
| `--apply --purge`, or `tmutil_delete` items | `0` for those items | increases | `purged` / `skipped` / `failed` |

`CleanItem.error` is an optional string present only when `status` is `skipped` or `failed`; values come from the `error` enum in §4.1 (e.g. `cross_volume`, `permission_denied`). The human output prints the same code with a hint.

`quarantine_path` is **optional**: it is absent in a dry run and in any run that quarantines
nothing (a `--purge` run, or a plan whose items are all `tmutil_delete`).

**Consistency rules (normative).** A `clean` document MUST satisfy all of them; the example above
does, and an implementation MUST reject one that does not:

- `planned_bytes` equals the sum of `items[].size_bytes` — the `items` array is never abridged.
- An item carries `error` only when its `status` is `skipped` or `failed`.
- In a dry run: `quarantined_bytes` and `reclaimed_bytes` are `0`, `quarantine_path` and
  `expired_sessions` are absent, and every item has `status: "planned"`.

> In a dry run `quarantined_bytes` and `reclaimed_bytes` are always `0` and every item is `planned`. This lets the GUI use the same code path to preview and to execute.

### 4.5 `quarantine list`

```json
{
  "data": {
    "quarantine_path": "/Users/x/.local/share/broza/quarantine",
    "total_bytes": 150600000000,
    "expired_bytes": 12400000000,
    "sessions": [{
      "id": "cln_20260917103608_a1b2",
      "created_at": "2026-09-17T10:36:08Z",
      "expires_at": "2026-10-17T10:36:08Z",
      "total_bytes": 138200000000,
      "item_count": 1284,
      "state": "complete"
    }, {
      "id": "cln_20260801091200_c3d4",
      "created_at": "2026-08-01T09:12:00Z",
      "expires_at": "2026-08-31T09:12:00Z",
      "total_bytes": 12400000000,
      "item_count": 37,
      "state": "expired"
    }]
  }
}
```

`state: "expired"` is derived at read time (`expires_at < now`) from a session whose stored state is `complete`; it is never written to the manifest. A manifest that stores it is rejected as corrupt.

`expires_at` is likewise **derived**: every reader recomputes it as `created_at` plus the current `quarantine-ttl`, so shortening the setting applies to sessions that are already in the store. The value in `manifest.json` is a cache, refreshed whenever a writer that knows the TTL rewrites the file; a reader never trusts it over the computed one.

The session object is also the shape of `manifest.json`, with one addition: in the manifest each
session carries `entries`, an array of the objects described in §4.6 (`id`, `original_path`,
optional `stored_path`, optional `restored_to`, `size_bytes`, `status`, optional `error`).
`quarantine list` omits `entries`; `restore --list` reports them under `items`.

### 4.6 `restore` and `quarantine expire|purge`

`restore` (also used for `restore --list`, where `items` carry `status: "planned"`):

```json
{
  "data": {
    "operation": "restore",
    "restored_bytes": 94200000000,
    "sessions": [{
      "id": "cln_20260917103608_a1b2",
      "status": "restored",
      "items": [{
        "id": "cln_20260917103608_a1b2/0001",
        "original_path": "/Users/x/Library/Developer/Xcode/DerivedData",
        "restored_to": "/Users/x/Library/Developer/Xcode/DerivedData",
        "size_bytes": 94200000000,
        "status": "restored"
      }]
    }]
  }
}
```

An item of a session has this shape:

| Field | Contract |
|---|---|
| `id` | `<session id>/<seq>`, the identifier accepted by `broza restore`. |
| `original_path` | Where the item was before it was quarantined. |
| `stored_path` | Optional. Where the item currently lives inside the session directory; present in `manifest.json`, omitted once the item was restored or purged. |
| `restored_to` | Optional. Where the item was put back; present after a restore, absent otherwise (and for `restore --list`). |
| `size_bytes` | Size of the item. |
| `status` | From the `status` enum in §4.1: `planned` for `restore --list`, then `restored`, `skipped` or `failed`. |
| `error` | Optional. Present only when `status` is `skipped` or `failed`. |

`quarantine expire` and `quarantine purge` share one shape:

```json
{
  "data": {
    "operation": "purge",
    "reclaimed_bytes": 138200000000,
    "sessions": [{
      "id": "cln_20260917103608_a1b2",
      "total_bytes": 138200000000,
      "item_count": 1284,
      "status": "purged"
    }]
  }
}
```

### 4.7 `explain`

```json
{
  "data": {
    "kind": "path",
    "volume": {
      "id": "disk3s5",
      "name": "Macintosh HD - Data",
      "role": "data",
      "mount_point": "/System/Volumes/Data",
      "used_bytes": 798210000000,
      "writable_by_broza": true,
      "purpose": "The writable volume that holds your home folder, your applications and your settings."
    },
    "path": "/Users",
    "explanation": {
      "what_it_is": "The mutable data volume of macOS. …",
      "what_it_is_for": "This is where practically everything that belongs to you lives. …",
      "is_it_safe": "Yes, with judgement. It is not a system volume. …"
    }
  }
}
```

| Field | Contract |
|---|---|
| `kind` | `volume` · `category` · `path`. Says which of the three optional target fields is present. |
| `volume` | Optional. The [`Volume`](#42-scan) object of §4.2. Present for `kind` `volume` and `path`, absent for `category`. |
| `category` | Optional. A value of the `category` enum of §4.1. Present only for `kind` `category`. |
| `path` | Optional. The target path, made absolute and cleaned of `.` and `..` **lexically** — nothing is followed, and `..` never climbs above `/`. Present only for `kind` `path`. |
| `filesystem` | Optional. The `type` of the container the volume belongs to (same open enum as §4.2), when Broza could determine it. The human header uses it to say `APFS role:` or `HFS+ role:` instead of assuming APFS. |
| `explanation` | Always present. Three prose fields: `what_it_is`, `what_it_is_for`, `is_it_safe`. English, one paragraph each; the same text the human output prints under its three headings. |
| `risk` | Optional. Base risk of the category (§3.3). Present only for `kind` `category`. |
| `action` | Optional. Default action of the category (§3.3). Present only for `kind` `category`. |

A `path` target is explained through the volume it lives on: `volume` and `path` are both present,
and the human output adds one line saying which volume the path is on. A path that does not exist
is not a target: it exits `4` like any other unknown target, rather than being answered with the
volume the working directory happens to be on.

`explain` supports `--json` and `--short`. It does **not** support `--csv` (exit `2`).

---

## 5. Donation message behaviour (RF-17)

Normative specification. Violating any of these rules is a *bug*.

**Shown only if all of the following conditions hold:**

1. The operation was a **successful** cleanup with `--apply` (`clean --apply`, `quarantine expire` or `quarantine purge` that freed or quarantined at least one byte).
2. stdout **and** stderr are both connected to an interactive TTY.
3. `--json`, `--csv` and `--quiet` are **absent**.
4. `config donate-prompt` is `true`.
5. `BROZA_NO_DONATE` and `CI` are **not** set.
6. It has not been shown in the last **30 days**.

Condition 6 is tracked by the marker file `~/.local/share/broza/state/donate_last_shown`, which contains a single RFC 3339 UTC timestamp and is rewritten (temp file + rename) each time the message is shown. A missing or unreadable marker counts as "never shown"; a write failure is logged at `-v` and never affects the exit code.

**Format (stderr, two lines maximum):**

```
  Broza made 138.2 GB reclaimable (121.4 GB in quarantine, 16.8 GB freed). It is free and open source software.
  If it helped you: https://ko-fi.com/broza   ·   Silence this: broza config set donate-prompt false
```

The figures follow principle 8: pending and freed bytes are reported separately; when one of them is zero its parenthesis is omitted.

**Explicit prohibitions:** never at startup, never on `scan` / `suggest` / `explain` / `restore` / `config` / `about`, never on a dry run, never on errors or exit codes other than `0`, never more than once per invocation, no animations or eye-catching colors.

---

## 6. Permission requirements

| Operation | Required permission |
|---|---|
| Enumerate disks and volumes | None |
| Scan `~` | None |
| Scan protected areas (Mail, Safari, Messages, backups) | **Full Disk Access** |
| List snapshots (`diskutil apfs listSnapshots`) | None |
| Delete snapshots (`tmutil deletelocalsnapshots`) | May require administrator privileges on macOS 26/27; Broza does not escalate |

If *Full Disk Access* is missing, Broza **does not fail**: it completes what it can, adds a warning to `warnings[]` and explains on stderr how to grant it: **System Settings → Privacy & Security → Full Disk Access**, then add the terminal application (or the `broza` binary). It returns exit code `3` only when the operation was impossible without it.

If `tmutil` refuses a snapshot deletion for lack of privileges, the item is marked `failed` with `error: "permission_denied"`, the exit code is `3` if no other item succeeded (else `5`), and stderr prints the exact `tmutil` command the user may run with `sudo`. Broza never invokes `sudo` itself.

---

## 7. Performance

| Scenario | Target |
|---|---|
| `scan` of the boot disk, cold cache | ≥ 100 000 entries/s (≈ 10 s for a 1 M-entry Data volume) |
| `scan` with warm cache | < 1 s |
| Full `suggest` | < 15 s |
| First visible result on screen | < 500 ms |

The cold budget is a rate because "ten seconds" says nothing without a disk
size: every entry costs at least one `stat`-equivalent, and a 3.7 M-entry home
directory cannot fit into ten seconds on any hardware Broza supports. A hundred
thousand entries per second is the rate that makes the original ten-second
figure true for the volume it was written for
([ADR 0006](adr/0006-scan-performance-budget.md)); `scripts/bench-scan.sh`
measures it.

Scanning is parallel per volume. The scan cache lives in `~/.cache/broza/v1/<volume_uuid>/`, one store per volume, keyed by `(dev, inode, mtime)` of each directory and expired by the configured `cache-ttl`. A directory whose key is unchanged is served from cache and its subtree is skipped. The store carries a versioned magic header; any decode error is reported as exit `9` (`CACHE_ERROR`) with the hint to retry with `--no-cache`, and Broza never attempts to "repair" a corrupt cache silently. `--no-cache` bypasses reads but still writes a fresh cache. A volume with no UUID falls back to its BSD name, with a `cache_keyed_by_bsd_id` warning. A subtree is only served from the cache when nothing inside it reaches `--min-size` — so a warm scan reports exactly what a cold one would, and `--min-size 0` disables the reuse entirely.

Cloud-provider roots (`~/Library/Mobile Documents`, `~/Library/CloudStorage`) are excluded from the walk by default: `lstat` on a file the provider has not downloaded blocks on that provider, and a scan that walks into them measures the network rather than the disk. They can be scanned explicitly by passing the path.

---

## 8. Resolved questions and deferred items

### 8.1 Resolved (binding for v1)

| # | Question (draft 1.0 §8) | Resolution |
|---|---|---|
| 1 | Orphan `node_modules` criterion | A `node_modules` directory is orphan when its parent has no `package.json` **or** the parent directory's mtime is older than `unused-after`. In monorepos only leaf `node_modules` are proposed. Tunable in a later minor. |
| 2 | Duplicate hashing | Three stages: group by size → compare the first 4 KiB → full BLAKE3 hash only on remaining candidates. |
| 3 | `last_used` reliability | `max(atime, kMDItemLastUsedDate)`. When Spotlight returns null (system apps) the finding's `reasoning` states low confidence. Native MDItem bindings may replace `mdls` later without contract changes. |
| 4 | Docker | v1 reports the allocated size of `Docker.raw` as an inform-only sub-finding of `build-cache`. No daemon or `docker` CLI calls. |
| 5 | Treemap in terminal | Not worth it in v1; tree plus usage bars suffice. `--treemap` is removed from this spec and reserved for the GUI. |
| 6 | Snapshot sizes | No public interface reports per-snapshot size. Findings report count, names and `purgeable` flag with `reclaimable_bytes: 0`. `com.apple.os.update-*` snapshots are never proposed. |
| 7 | `tmutil deletelocalsnapshots` privileges | v1 does not escalate. Permission denied → item `failed`, exit `3`, instruction printed. Actual need for root on macOS 26/27 is verified during implementation. |

### 8.2 Deferred (post-1.0, not part of this specification)

- **Treemap** (`--treemap`, RF-05 treemap portion) — reserved for the Phase 2 GUI.
- **Scheduled cleanups / `broza schedule` via `launchd`** (RF-10) — conflicts with the no-TTY rule (exit `7`) and needs its own consent model and spec section.
- **Native DiskArbitration adapter** — v1 parses `diskutil … -plist` behind an enumeration trait; the native adapter can replace it without contract changes.
- **Per-volume quarantine roots** — automatic quarantine directory on each writable volume, removing the cross-volume skip. v1 offers the single configurable `quarantine-path`.
- **Docker daemon integration** — querying image, container and volume sizes through the Docker API, and proposing `docker system prune` as an executable action.
- **APFS clone-aware sizes**, HFS+ specifics beyond enumeration, FAT/exFAT support.

---

## 9. Changelog 1.0 → 1.1

- Document rewritten in English; all examples, JSON strings and UI text in English with decimal point (`138.2 GB`, `1.00 TB`). Status "Approved for implementation"; platform pinned to macOS 26/27, Apple Silicon.
- §0: added principle 8 ("Honest about quarantine": pending vs freed bytes always shown separately).
- §1: `--csv` restricted to `scan`, `suggest`, `quarantine list`, `restore --list` (else exit `2`); `--json` + `--csv` is a usage error. New §1.4 with the duration grammar (`h`, `d`, `w`, `m` = months, `y`) and size grammar (decimal `KB…TB` and binary `KiB…TiB` on input; decimal on output).
- §2: explicit exit-code rows for `clean --apply` with red/`inform_only` items (`2`), `--yes` + `--purge` (`2`), `--csv` on unsupported commands (`2`), cross-volume skips (`skipped` + `5`), `quarantine expire` with nothing to do (`0`).
- §3.1 `scan`: `--include-external` replaced by `--no-external` (default: externals included); `--treemap` removed (deferred); percentages defined at container level.
- §3.2 `explain`: category ids accepted as targets (`broza explain cloud-synced`, `broza explain snapshots`); resolution order defined.
- §3.3 `suggest`: category table gains an "Action" column; normative notes for `snapshots` (`reclaimable_bytes: 0`, names + `purgeable`, never `com.apple.os.update-*`), `Docker.raw` inform-only sub-finding, orphan `node_modules` criterion, duplicate hashing and `last_used` confidence; text risk labels mandatory.
- §3.4 `clean`: automatic expiry of past-TTL quarantine sessions on `--apply` (green-level confirmation, covered by `--yes`); cross-volume rule (`skipped` + `cross_volume` + hint); explicit safety-check order and allowed roots; `--max-size` breach exits `2`.
- §3.5 `restore`: id forms defined; collision → `skipped` unless `--to`; manifest layout and per-session atomicity made explicit; `--list` supports `--csv`.
- §3.6 `config`: TOML example with English comments; `set` validates types; `quarantine-path` same-device note.
- §3.7 `about`: example output and `--json` support.
- §3.8 **new** `broza quarantine list|expire|purge` command; documents that space is freed only on expiry or purge.
- §4: `schema_version` bumped to `1.1`; stable enum list added (`role`, `risk`, `action`, `status`, `state`, `category`, `error`); `errors[]`/`warnings[]` entry shape defined. §4.3 gains a `snapshots` finding example. §4.4 `clean` gains `quarantined_bytes`, `expired_sessions`, optional `CleanItem.error`, and normative byte-counter definitions per mode. New §4.5 (`quarantine list`) and §4.6 (`restore`, `quarantine expire|purge`).
- §5: donation conditions and message translated; message reports pending and freed bytes separately; state marker `~/.local/share/broza/state/donate_last_shown` specified; prohibitions extended to `restore`, `config`, `about`.
- §6: Full Disk Access path in English; snapshot deletion privileges and no-escalation policy documented.
- §7: cache location `~/.cache/broza/v1/<volume_uuid>/`, key `(dev, inode, mtime)`, TTL `cache-ttl`, corruption → exit `9`.
- §8: former "open questions" resolved (node_modules, hashing, last_used, Docker, treemap, snapshot sizes, tmutil privileges) and a deferred list added (treemap, schedule/launchd, native DiskArbitration, per-volume quarantine roots, Docker daemon integration).
- §9: this changelog.
- §4 (while 1.1 is unreleased, so no bump): documented what the model of `crates/broza/src/model/` already implements — optional `volumes[].mount_point` (unmounted `Preboot` / `Recovery`), optional `snapshots[].uuid`, optional `clean.quarantine_path`, the `entries` array of `manifest.json` and the item fields `stored_path` / `restored_to`; `type` added to the stable-enum table with pass-through of unknown tokens; unknown-value handling made explicit per enum (verbatim pass-through for the persisted ones, collapse to `unknown` for `role`); `instructions` required for every `inform_only` finding; normative consistency rules for `clean`, and its example renumbered so `planned_bytes` is the sum of the items shown.
- §2 (M1 safety kernel, unreleased): explicit exit-code rows for safety-kernel refusals (`2`), vanished items (`skipped` + `not_found`), OS permission errors on single items (`failed` + `permission_denied`), and dry runs whose selection is entirely `inform_only` (`0` with a warning).
- §3.4 (M1, unreleased): confirmation row for natively irreversible actions (`trash`, `snapshots`) without `--purge`.
- §3.1, §3.2, §4.7 (M2 read-only disk, unreleased): the `scan` and `explain` sketches now show the
  output the implementation actually produces — sizes follow the precision of §1.4 (one decimal
  below a terabyte, so `798.2 GB`, not `798.21 GB`), the free line is labelled `Free`, and the
  purgeable line reads `← estimate; macOS shows this as "available"`. §3.1 documents the
  `(external)` marker, the `HFS+ volume` label, the volume-row notes and the continuation gutter
  drawn for a disk with more than one container. New §4.7 specifies the `explain` payload
  (`kind`, optional `volume` / `category` / `path` / `filesystem`, `explanation`, optional `risk` /
  `action`), states that a path target is resolved lexically and must exist, and records that
  `explain` has no `--csv` form.
- §3.1 (M2, unreleased): **`scan --csv` emits the volume table**, header
  `disk_id,container_id,volume_id,name,role,mount_point,used_bytes,writable_by_broza`, and not the
  flattened `largest_items` table draft 1.1 named. The two tables sit at different levels — one row
  per volume against one row per path — so a single file holding both would have no stable column
  set, and `largest_items` does not exist until the folder walker lands. It stays available through
  `--json`.
- §3.1 (M2, unreleased): `--volume` accepts a device id, a volume name or a mount point, the same
  three forms `explain` accepts, matched exactly. A blank value is a usage error (`2`); any other
  value that names nothing is `4`. The folder-walking inputs (`PATH`, `--depth`, `--top`,
  `--min-size`, `--tree`) are accepted and raise the warning `folder_scan_pending`, which
  disappears once the walker is wired.
- §4.1 (M3, unreleased): item error codes `changed_since_check` and `max_size_exceeded` added.
- §4.2 and §7 (M2 scanner, unreleased, so no bump): APFS clone accounting stated as best-effort and deferred to post-1.0 (PRD RF-02), cloud placeholders (`SF_DATALESS`) documented as 0 bytes and never listed; new optional `volumes[].uuid`, which the scan cache is filed under, with a `cache_keyed_by_bsd_id` warning when it is missing; cloud-provider roots excluded from the walk by default; §7 states that a subtree is only reused from the cache when nothing in it reaches `--min-size`, which `--min-size 0` therefore disables.
- §7 (M2, unreleased): the cold-scan budget is restated as a throughput — at least 100 000 entries per second, which is the ten seconds the table always named, for a one-million-entry volume ([ADR 0006](adr/0006-scan-performance-budget.md)). The warm and first-result budgets are unchanged.
- §3.1 (M2, unreleased): the usage bar never rounds a container that is in use down to an empty bar, nor one with room left up to a full one; a 99.8% full container keeps its last free cell.
- §4.1 (M3, unreleased): item status `moving` added, for the manifest only.
- §3.4 (M3, unreleased): `--max-size` spelled out as two checks — exit `2` before execution on the
  scanned total, `max_size_exceeded` per item after re-measurement — and directories are only
  re-measured when a cap is given.
- §3.5 (M3, unreleased): "atomic per session" replaced. A restore moves each item out of the
  manifest as it succeeds, leaves what failed in quarantine with `state: restoring`, and deletes
  the session only when its directory is empty.
- §4.5 (M3, unreleased): `expires_at` documented as derived from `created_at` plus the current
  `quarantine-ttl`; the stored value is a cache.
- §4.1 (M3, unreleased): the envelope codes of the quarantine store are listed.
- §3.5/§3.8 (M3, unreleased): every operation that writes to a session holds `<session>/.lock`
  for its duration; a session another Broza holds is reported `session_busy` and left alone.
