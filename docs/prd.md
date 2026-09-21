---
product: Broza
type: Product Requirements Document
version: 1.0
status: Approved for implementation
date: 2026-09-21
author: Borja Lafuente Romero
platform: macOS (2 latest versions: 26 and 27), Apple Silicon
license_core: MIT
language_core: Rust
related_docs:
  - docs/cli-spec.md (CLI specification v1.1)
  - docs/implementation-plan.md
  - docs/adr/
---

# PRD: Broza

> **Note for AI agents.** This document is self-contained and normative. Closed decisions live in §11 and **must not be reopened** without explicit instruction. Requirements are atomic and identifiable (`RF-xx`, `RNF-xx`) and their IDs are stable. The safety requirements in §7.3 are **invariants**: no optimization, refactor or later request overrides them. The CLI surface is specified separately in `docs/cli-spec.md`; milestones and architecture in `docs/implementation-plan.md`.

---

## 1. Summary

**Broza** is a macOS utility that answers the three questions every Mac user asks when the disk fills up:

1. **What do I have?** It analyzes disks, APFS containers, volumes and partitions.
2. **What is each thing for?** It explains them in plain language.
3. **What can I delete without breaking anything?** It recommends safe, reversible cleanup.

**Two-phase strategy:**

| | Phase 1 | Phase 2 |
|---|---|---|
| Product | Open source CLI (MIT) | Desktop GUI |
| Audience | Power users / developers | General users |
| Model | Free + voluntary donation | 5 EUR/USD per year |
| Distribution | Homebrew | Notarized DMG, outside the App Store |
| Goal | Adoption and validation | Revenue |

**The name.** "Broza" is Spanish for brushwood or leaf litter and, figuratively, *waste* or *a heap of useless things* (RAE). It was chosen after discarding nine English candidates because of Class 9 trademark collisions or conflicts with existing tools. Pronounced BRO-zah.

---

## 2. Problem

- **A Mac's SSD is expensive and not upgradeable.** On Apple Silicon it is soldered: "buying more space" means buying another Mac.
- **macOS hides the problem.** Finder shows "Available" = free + *purgeable*, a figure that does not match what an app sees via `statvfs`. This creates permanent confusion.
- **"System Data" is a black box** of tens of GB: APFS snapshots, swap, caches, Apple Intelligence models.
- **Manual guides are dangerous:** they involve Terminal, `tmutil` and deleting inside `~/Library`. One mistake destroys real data.

**Differentiation.** The market is polarized between visualizers that do not clean (DaisyDisk) and cleaners perceived as opaque. Broza combines **honest explanation + safe, auditable cleanup**, with an open source core that provides the trust this category needs.

---

## 3. Users

### 3.1 Dana, developer (**sole target user of v1**)

- 512 GB MacBook Pro saturated with DerivedData, iOS simulators, `node_modules`, Docker images.
- Wants to reclaim space fast from Terminal, with full control and the ability to script.
- Values: auditable code, *dry-run* by default, Homebrew, CI integration.

### 3.2 Marcos, general user (**out of scope for v1**)

- Family iMac, 90 GB of "System Data", old iPhone backups.
- Wants a visual app that tells him in plain language what is safe to delete.
- Documented only to orient Phase 2. **He constrains no v1 decision.**

---

## 4. Scope

### 4.1 Included

- Analysis of mounted internal and external disks, APFS containers, volumes with role, partitions and snapshots.
- File systems: **APFS and HFS+**, with an extensible plugin architecture.
- Explanation of the purpose of each volume.
- Detection and recommendation of safe cleanup.
- Rust core + CLI (Phase 1) + GUI (Phase 2).

### 4.2 Excluded from v1

- Recovery of deleted data or disk forensics.
- Antivirus, "accelerators", performance optimization.
- Destructive partitioning or resizing (read-only reporting only).
- FAT/exFAT/NTFS and network disks.
- Windows and Linux.
- Disk repair / `fsck` (Disk Utility covers it).
- **Any sales, licensing or tax infrastructure.** Phase 1 sells nothing.

---

## 5. Technical context (macOS)

### 5.1 APFS hierarchy

```
Physical disk -> APFS container -> Volumes -> Snapshots
```

Volumes in a container **share space dynamically** (*space sharing*): free space belongs to the container, not to the volume.

> **Product consequence:** never show "% full" per volume as if volumes were partitions. Always reason at container level.

### 5.2 Volume roles

| Role | Function | Writable by Broza? |
|---|---|---|
| `system` | Read-only, sealed and signed OS (SSV) | No. **Never** |
| `data` | `/Users`, apps, `~/Library`, caches. Joined to `system` via *firmlinks* | Yes. 99% of what is cleanable |
| `preboot` | Container boot files | No. **Never** |
| `vm` | Virtual memory swap | No. **Never** (shrinks after a reboot) |
| `recovery` | recoveryOS | No. **Never** |
| `backup` | Time Machine store | Only via `tmutil` |

### 5.3 Purgeable space and "System Data"

- **Purgeable:** macOS reports "Available" = free + purgeable. Broza must show **both figures separately** and explain the difference. Never promise purgeable as immediately free space. Broza obtains purgeable as an **estimate** (see RF-01) and labels it as such.
- **APFS local snapshots:** *copy-on-write* copies from Time Machine (`com.apple.TimeMachine.*`) and from OS updates (`com.apple.os.update-*`). They are counted as "System Data" and can occupy tens of GB. List them with `diskutil apfs listSnapshots -plist` (exposes the `Purgeable` flag) or `tmutil listlocalsnapshots`; delete them **exclusively** with `tmutil deletelocalsnapshots`, never by deleting files by hand. macOS reports no per-snapshot size.
- **Apple Intelligence models:** several GB that are only released by disabling the feature. **Inform, do not delete.**

### 5.4 Permissions: TCC vs. sandbox

| Mechanism | What it allows | Relevance |
|---|---|---|
| **Full Disk Access (TCC)** | A *non-sandboxed* app may read protected areas | What the Broza GUI will use |
| **App Sandbox** | Mandatory on the Mac App Store. Enforced in the kernel and **not disabled by Full Disk Access** | Reason the App Store is ruled out (D1) |

---

## 6. Architecture

```
+------------------------------+   +------------------------------+
|  CLI (Rust, MIT)             |   |  GUI (SwiftUI, proprietary)  |
|  Phase 1                     |   |  Phase 2                     |
+--------------+---------------+   +--------------+---------------+
               |                                  |
               |              +-------------------+
               |              |  FFI bridge (C-ABI + swift-bridge/UniFFI)
               v              v
+-------------------------------------------------+
|  Core Engine (Rust, MIT), crate `broza`         |
|  . Disk enumeration (`diskutil -plist` behind   |
|    a `DiskEnumerator` trait; DiskArbitration    |
|    adapter optional later)                      |
|  . Parallel scan + cache                        |
|  . Detectors (one plugin per category)          |
|  . Action engine + quarantine                   |
+-------------------------------------------------+
               |
               v
+-------------------------------------------------+
|  Privileged helper (XPC/SMJobBless), optional   |
|  Phase 2 only; v1 never escalates privileges    |
|  Only possible outside the App Store            |
+-------------------------------------------------+
```

Layout: Cargo workspace with `crates/broza` (core library, published on crates.io as `broza`) and `crates/broza-cli` (binary `broza`). The core never touches stdin/stdout or the environment directly; it receives its dependencies (process runner, disk enumerator, file operations, clock, prompter) as traits so the same core serves CLI, tests and the future GUI.

**Why Rust and not Go:** no runtime or garbage collector, clean C-ABI, mature Swift interop tooling (`swift-bridge`, UniFFI) and safe parallelism for scanning. One core feeds CLI and GUI.

**Why MIT:** allows building the proprietary GUI on the same core (*open core* model). GPL would force releasing the GUI too.

---

## 7. Functional requirements

### 7.1 Analysis (Phase 1)

| ID | Requirement |
|---|---|
| **RF-01** | Enumerate physical disks, APFS containers, volumes (with role), partitions and snapshots by parsing `diskutil … -plist` output (`diskutil list`, `diskutil apfs list`, `diskutil info`, `diskutil apfs listSnapshots`) behind a `DiskEnumerator` trait. A native DiskArbitration adapter may be added later without changing callers. Purgeable space is estimated via Foundation `NSURL` `volumeAvailableCapacityForImportantUsage` (purgeable = important-usage capacity minus available capacity, clamped at 0) and reported as an estimate. |
| **RF-02** | Compute real usage per volume and container, distinguishing real free space from purgeable. **Count hard links only once** (dedupe by device + inode). APFS clones are also counted once where detectable; clone-aware accounting is best-effort in v1 and completed post-1.0 (see §16). |
| **RF-03** | Hierarchical scan by folder and type, with a cache for fast re-scans. |
| **RF-04** | Translate technical volume roles into plain-language explanations. |
| **RF-05** | Tree view with colors and usage bars per container. (Unicode treemap deferred to the GUI, see D15.) |
| **RF-06** | Human output plus `--json` / `--csv` for scripting and for feeding the GUI. The JSON envelope is the contract between the Rust core and the future GUI: sizes are always integer bytes, timestamps RFC 3339 UTC, schema versioned with semver. |

### 7.2 Cleanup detectors

**Guiding principle:** the flow is always **explain -> suggest -> (if the user confirms) clean**.

| Category ID | Detects | Risk | Action |
|---|---|---|---|
| `user-cache` | `~/Library/Caches`, logs, incomplete downloads | green | quarantine |
| `build-cache` | DerivedData, Archives, orphan `node_modules`, stale Docker, `__pycache__`, `.gradle`, `target/` | green/amber | quarantine |
| `ios-simulators` | Unused iOS simulators | amber | quarantine |
| `trash` | Trash folders on all volumes | amber | purge |
| `snapshots` | APFS local snapshots | amber | `tmutil` |
| `old-backups` | iOS backups in `MobileSync/Backup` | amber | quarantine |
| `unused-apps` | Apps not opened for longer than the threshold + leftovers in `~/Library` | amber/red | quarantine |
| `cloud-synced` | Files already in iCloud / Dropbox / OneDrive / Google Drive | red | **inform only** |
| `duplicates` | Identical copies by hash | amber | quarantine |
| `large-old-files` | Large files not opened for a long time | amber | quarantine |

Detector notes (normative for v1):

- **`snapshots`:** macOS reports no per-snapshot size. The finding reports the count, the snapshot names and the `Purgeable` flag; `reclaimable_bytes` is `0` with `reasoning` "size not reported by macOS". Broza never proposes `com.apple.os.update-*` snapshots (`Purgeable = false`). Deletion goes only through `tmutil deletelocalsnapshots`; v1 does not escalate privileges. If permission is denied the item is `failed` and the command exits `3` with an instruction.
- **`build-cache`, Docker:** v1 reports the allocated size of `Docker.raw` as an inform-only sub-finding. No calls to the Docker daemon.
- **`build-cache`, orphan `node_modules`:** a `node_modules` directory is orphan when its parent has no `package.json`, or when the parent directory's mtime is older than `unused-after`. In monorepos only leaf `node_modules` are proposed. Criterion tunable later.
- **`duplicates`:** candidates grouped by size, then by the first 4 KiB, then by a full `blake3` hash only for remaining candidates.
- **`unused-apps`, `large-old-files`:** `last_used` = max(`atime`, `kMDItemLastUsedDate`). When Spotlight returns no date (typical for system apps) the finding's `reasoning` is marked low confidence.
- **`unused-apps`:** threshold 1 year, configurable (D4). Leftovers in `~/Library` are matched by bundle identifier and only suggested.
- **`cloud-synced`:** non-actionable by construction (RF-19, D3).

### 7.3 Safety requirements: **INVARIANTS**

> These rules admit no exception, override flag or later optimization.

| ID | Requirement |
|---|---|
| **RF-07** | **Dry-run by default.** Nothing is written without `--apply`. |
| **RF-08** | Every deletion goes to **reversible quarantine** by default (TTL 30 days, configurable). Only actions that cannot be quarantined bypass it: `purge` for `trash` (already user-discarded) and `tmutil_delete` for `snapshots`; both require confirmation per RF-18. Quarantine is a same-volume rename: it frees no space until the item expires or is purged. `clean` reports `quarantined_bytes` (pending) separately from `reclaimed_bytes` (freed now). `broza quarantine list \| expire \| purge` manages the store; `clean --apply` also expires sessions past their TTL under the same confirmation policy as green items. Items on a volume different from the quarantine root are `skipped` with a `cross_volume` error and a hint (set `quarantine-path` on that volume, or use `--purge`). Irreversible deletion requires `--purge` plus typing the literal `PURGE`; `--yes` is ignored. |
| **RF-09** | Exclusions and *allowlist*. **Never write to `system`, `preboot`, `recovery` or `vm` volumes.** No flag allows it. |
| **RF-18** | **No deletion without explicit user confirmation.** No TTY and no `--yes` -> exit with code `7`. |
| **RF-19** | The `cloud-synced` category is **non-actionable**. Broza never deletes cloud-synced files; it only informs and shows the provider's official steps. |

### 7.4 Other Phase 1 requirements

| ID | Requirement |
|---|---|
| **RF-10** | **Deferred to post-1.0.** Scheduled cleanups (`launchd`). Reason: unattended execution conflicts with RF-18 (no TTY and no `--yes` -> exit `7`) and a future `broza schedule` command needs its own specification section. Configuration profiles (e.g. `developer`) remain in v1 (M5). |
| **RF-17** | Discreet support message (Ko-fi) on stderr, silenceable, never in CI nor with `--json`. Specified in `docs/cli-spec.md` §5. |

### 7.5 Phase 2: GUI

| ID | Requirement |
|---|---|
| **RF-11** | SwiftUI app consuming the Rust core via FFI, with interactive treemap and *drill-down*. |
| **RF-12** | "Review -> select -> clean" flow with visible quarantine and one-click undo. |
| **RF-13** | Explained "System Data" panel with guided safe actions. |
| **RF-14** | Full Disk Access onboarding and license-key activation. |
| **RF-15** | Scheduled cleanups, space monitor and nearly-full disk alerts. |
| **RF-16** | Paywall: analysis is free; **executing** cleanup requires a license. |

---

## 8. Non-functional requirements

| ID | Attribute | Requirement |
|---|---|---|
| **RNF-01** | Data safety | Dry-run, quarantine, confirmation proportional to risk, untouchable system volumes. |
| **RNF-02** | Performance | Warm `scan` < 1 s; cold < 10 s; `suggest` < 15 s; first visible result < 500 ms. |
| **RNF-03** | Privacy | No telemetry by default. If it ever exists: opt-in, anonymous, documented. **Zero exfiltration of file names or content.** |
| **RNF-04** | Compatibility | The two latest macOS major versions (26 and 27 at the date of this document); Apple Silicon only. Policy: support always follows the "two latest majors" window. |
| **RNF-05** | Supply chain | Signed and notarized binaries; reproducible build; SBOM. |
| **RNF-06** | Accessibility (GUI) | VoiceOver, Dynamic Type, contrast. **Never rely on color alone** to communicate risk; the CLI always pairs color with a text label. |
| **RNF-07** | i18n | CLI in English, including all output and JSON text fields; GUI localized (ES/EN) in Phase 2. |

---

## 9. Success metrics

> In Phase 1 the goal is **not revenue** but adoption, trust and validation of the engine.

| Goal | KPI |
|---|---|
| Accuracy | Deviation < 5% between estimated and actually reclaimed space |
| Safety | 0 incidents of unconfirmed irreversible deletion |
| Value | Median >= 10 GB made reclaimable (quarantined + reclaimed) on the first pass |
| **Adoption (main Phase 1 KPI)** | GitHub stars, Homebrew installs, contributors |
| Donations | A signal of appreciation, not of business |
| Validation | Traction threshold that justifies building the GUI (e.g. 1,000 active installs) |
| Conversion (Phase 2) | % of CLI users who buy; annual renewal |

---

## 10. Monetization

**Guiding principle: do not build business infrastructure before there is a business.**

| | Phase 1 | Phase 2 |
|---|---|---|
| Model | Free, MIT, voluntary donation | License 5 EUR/USD per year |
| Payment | Ko-fi (Stripe already connected) | Merchant of Record (to be chosen) |
| Consideration | **None.** Pure donation | Access to cleanup in the GUI |
| Infrastructure | Zero: one link | Keys, notarization, Sparkle |
| When | From day 1 | Only if Phase 1 shows traction |

### 10.1 Ko-fi configuration

- **Disable "Contributor status"** under Settings -> Payment. It is on by default and gives up 5% of tips. Disabled, one-off donations carry 0% platform fee.
- Stripe processing (~2.9% + $0.30) is always paid. The **fixed part** weighs on small amounts: on $5 it is ~$0.45 (about 9%).
- **Design consequence:** suggest amounts of $5 or more; do not leave the amount open downwards.
- **Offer nothing in return.** Keeping it a pure donation separates it cleanly from the sales channel.

### 10.2 Why a Merchant of Record in Phase 2

| Criterion | Stripe (processor) | Merchant of Record |
|---|---|---|
| Legal seller | **You** | The provider |
| Tax liability | Yours: register, file, remit | The provider's |
| Chargebacks | You absorb them | The provider absorbs them |
| Indicative cost | 2.9% + $0.30 (+0.5% with Stripe Tax) | 5% + $0.50 |

- **A Stripe account is not a MoR.** Stripe processes; the legal seller is still you. **Stripe Tax calculates and collects, but does not assume the obligation** to register, file and remit.
- Selling digital licenses to EU consumers from outside the EU, **the VAT obligation arises with the first sale, with no threshold**. The alternative to a MoR is registering under the *Non-Union OSS* scheme and filing every quarter indefinitely: disproportionate for three-figure revenue.
- A MoR **has no fixed cost**: you pay only when you sell.
- Stripe acquired Lemon Squeezy (2024) and launched Stripe Managed Payments (2026), which charges an **additional 3.5%** on top of processing fees. Compare with Lemon Squeezy and Paddle when the time comes rather than assuming Stripe is the natural choice.

### 10.3 Tax situation

- **Canada (GST/HST):** below **30,000 CAD** of **worldwide** taxable revenue the *small supplier* status holds, with no registration obligation. Measured on **gross** revenue over a rolling 4-quarter window.
- **EU / United Kingdom:** not applicable in Phase 1 because **there is no sale**. The VAT obligation arises with a sale, not with a donation. One more reason to keep Ko-fi without consideration.

> **To verify with an advisor: Beckham law.** The Spanish impatriate regime **does not exempt all foreign income indiscriminately**: it exempts certain foreign-source capital income, while worldwide employment income and qualifying business activity income **are taxed in Spain**. Its fit for self-employed activity is also restricted. Selling software licenses is **business activity, not passive income**. Consult a specialized advisor **before enabling any sale**. This document is not tax advice.

---

## 11. Closed decisions

> **Do not reopen without explicit user instruction.**

| ID | Decision |
|---|---|
| **D1** | Phase 1: free + Ko-fi with no consideration. Phase 2: paid GUI **outside the App Store** with a MoR. No sales infrastructure until Phase 2. |
| **D2** | v1 is **only** for power users comfortable with a CLI. |
| **D3** | `cloud-synced`: **recommend only** + official instructions. Never delete. |
| **D4** | `unused-apps`: threshold **1 year** (configurable 6m/2y). Suggest leftovers in `~/Library`, never delete without confirmation. |
| **D5** | Core in **Rust**, **MIT** license. |
| **D6** | Phase 2 paywall: 1 year for 5 EUR/USD. Re-evaluate the price when the time comes. |
| **D7** | Only the **two latest macOS versions**, only **Apple Silicon**. |
| **D8** | v1 with **APFS and HFS+**; extensible to FAT/exFAT. |
| **D9** | Name: **Broza**. |
| **D10** | Billing jurisdiction: **Canada**. |
| **D11** | **English everywhere:** CLI output (including JSON text fields), code, comments, commits, documentation. Decimal point in numbers (`138.2 GB`). |
| **D12** | v1 disk enumeration parses `diskutil … -plist` behind a `DiskEnumerator` trait. A native DiskArbitration adapter is optional post-1.0 and must not change callers. |
| **D13** | Quarantine is a same-volume rename and frees space only on expiry or purge. `broza quarantine list \| expire \| purge` exists; `clean` reports `quarantined_bytes` and `reclaimed_bytes` separately; cross-volume items are skipped with a hint. |
| **D14** | RF-10 (scheduled cleanups via `launchd`) is deferred to post-1.0; a future `broza schedule` command needs its own spec section. |
| **D15** | Treemap deferred to the GUI. v1 terminal output is tree view + usage bars; no `--treemap` flag in v1. |

---

## 12. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Data loss | Critical: kills trust | Dry-run, quarantine, mandatory confirmation, cloud inform-only |
| Intrusive donation message | Medium: uninstalls, bad reputation | Strict RF-17: stderr, silenceable, never in CI |
| Wrong tax assumption (Beckham) | High: back taxes and surcharges | Verify with an advisor before enabling sales |
| Turning the donation into a sale by accident | Medium: triggers VAT | Offer nothing in return on Ko-fi |
| Name SEO (David Broza) | Medium: discoverability | Position "broza cli", "broza mac cleaner"; domain `broza.app` |
| Miscalculation (clones/hard links) | Medium: credibility | Count once; validate against Disk Utility |
| macOS APIs change | Medium: maintenance | Rely on `tmutil` and `diskutil` plist output; plist fixtures and tests per macOS version |
| Quarantine misunderstood as "freed" | Medium: trust in figures | Separate `quarantined_bytes` from `reclaimed_bytes`; `quarantine` command; KPI wording "made reclaimable" |

---

## 13. Roadmap

### Phase 0: reserve names (urgent, irreversible)

1. Publish an empty `broza` crate on crates.io.
2. GitHub organization.
3. Domain `broza.app` (`.com` is listed at ~$7,900).
4. Ko-fi account **with Contributor status disabled**.

### Phase 1: open source CLI (milestones M0-M5)

Each milestone ends with tests green, coverage >= 80%, clippy clean and a tag. Details in `docs/implementation-plan.md`.

1. **M0 Skeleton + contract:** Cargo workspace, serde model types (frozen JSON contract), exit codes, size/duration parsers, full command tree, CI on macOS 26 and 27, `cargo-dist`. Nothing reads disks.
2. **M1 Safety kernel:** `Approved<T>` write token, confirmation policy matrix, path canonicalization, protected-role rejection, root allowlist, `--max-size`, port traits and fakes, dry-run planner.
3. **M2 Read-only disk (release 0.1):** `diskutil` plist adapters with fixtures, mount table, NSURL purgeable estimate, parallel walker with hard-link dedupe, scan cache, `scan` and `explain`. Homebrew tap.
4. **M3 Green detectors + quarantine (release 0.2):** `user-cache`, `build-cache`, quarantine store/restore/expiry, `clean --apply`, `quarantine`, `restore`, donation gate (RF-17), `suggest`.
5. **M4 Amber detectors:** `trash`, `snapshots`, `old-backups`, `ios-simulators`, `duplicates`, `large-old-files`.
6. **M5 Inform + apps + polish (release 1.0):** `cloud-synced`, `unused-apps`, profiles, Full Disk Access warning path, SBOM, reproducible build, complete docs.

**Deferred (post-1.0):** RF-10 `schedule`, treemap, native DiskArbitration adapter, APFS clone-aware sizes, per-volume quarantine roots, native MDItem bindings, Docker daemon integration, HFS+ specifics beyond enumeration, FAT/exFAT.

### Decision point

Evaluate real traction against the §9 threshold **before** investing in the GUI. If Phase 1 does not generate adoption, a paid GUI will not fix it.

### Phase 2: paid GUI

1. Rust <-> Swift FFI bridge + SwiftUI GUI (RF-11, RF-12).
2. "System Data" panel and onboarding (RF-13, RF-14).
3. Monitor, paywall, notarization, Sparkle, MoR registration (RF-15, RF-16).

---

## 14. Pending tasks (non-blocking for development)

- [ ] Choose the concrete MoR (Lemon Squeezy / Paddle / Stripe Managed Payments), Phase 2 only.
- [ ] Consult an impatriate-regime advisor about the Beckham law, before selling.
- [ ] Register crate, GitHub organization, domain, Ko-fi.
- [ ] Formal trademark search (CIPO, EUIPO, USPTO).

---

## 15. Assumptions

- Modern macOS (26 and 27 at the time of writing) with APFS as the main file system; HFS+ supported; FAT/exFAT postponed.
- The developer / power user is the **only** v1 user; the CLI precedes and validates the GUI.
- Phase 1 **generates no relevant revenue**; its goal is adoption and validation.
- Fee figures, rates and thresholds are indicative as of the document date.
- **This PRD is not tax or legal advice.**

---

## 16. Requirements traceability

| ID | Milestone | Notes |
|---|---|---|
| RF-01 | M2 | `diskutil` plist adapters + NSURL purgeable estimate |
| RF-02 | M2 | Hard-link dedupe by `(dev, inode)`; APFS clone accounting best-effort, completed post-1.0 |
| RF-03 | M2 | Walker + scan cache |
| RF-04 | M2 | `explain` texts for roles, paths, categories |
| RF-05 | M2 | Tree + usage bars; treemap deferred (D15) |
| RF-06 | M0, M2, M3 | Contract frozen in M0; `scan`/`explain` in M2; `suggest`/`clean` in M3 |
| RF-07 | M1, M3 | Policy in M1; executor in M3 |
| RF-08 | M3 | Quarantine store, `quarantine` command, `restore`, expiry |
| RF-09 | M1 | Role rejection, root allowlist, exclusions |
| RF-10 | Deferred | Post-1.0 (D14) |
| RF-11 to RF-16 | Phase 2 | GUI |
| RF-17 | M3 | Donation gate, all six conditions + 30-day marker |
| RF-18 | M1, M3 | Confirmation matrix in M1; exit `7` path end-to-end in M3 |
| RF-19 | M1, M5 | `inform_only` forced by construction in M1; `cloud-synced` detector in M5 |
| Detectors green | M3 | `user-cache`, `build-cache` |
| Detectors amber | M4 | `trash`, `snapshots`, `old-backups`, `ios-simulators`, `duplicates`, `large-old-files` |
| Detectors red/apps | M5 | `cloud-synced`, `unused-apps` |
| RNF-01 | M1, M3 | Structural safety kernel + quarantine |
| RNF-02 | M2, M4 | `scan` benchmarks in M2; `suggest` < 15 s in M4 |
| RNF-03 | M0 to M5 | No network code in v1; verified at M5 |
| RNF-04 | M0 | CI matrix macOS 26 + 27, arm64 only |
| RNF-05 | M0, M5 | `cargo-dist` in M0; SBOM, `--locked` reproducible build, signing in M5 |
| RNF-06 | Phase 2 | GUI accessibility; CLI text labels for risk from M2 |
| RNF-07 | M0, Phase 2 | English CLI from M0 (D11); GUI localization in Phase 2 |
