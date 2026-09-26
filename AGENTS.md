# AGENTS.md — Broza

Guidance for AI coding agents and automation working in this repository.
Humans: read [README.md](README.md) first. This file is normative for agents.

## 1. Purpose

Broza is a macOS CLI (Rust, MIT) that analyzes APFS storage, explains what each volume and
folder is for, and reclaims space through dry-run, reversible cleanup. Phase 1 is the CLI only.

Source of truth, in priority order:

1. [docs/prd.md](docs/prd.md) — requirements (`RF-xx`, `RNF-xx`), closed decisions (`D1`–`D15`).
2. [docs/cli-spec.md](docs/cli-spec.md) — CLI surface, exit codes, JSON contract.
3. [docs/implementation-plan.md](docs/implementation-plan.md) — architecture and milestones.
4. [docs/adr/](docs/adr/) — architecture decision records.

## 2. Non-negotiable invariants

These rules admit no exception, override flag, or later optimization. Any change that weakens
one of them is rejected, regardless of who asks.

1. **Dry-run by default.** Nothing is written to disk without `--apply`.
2. **Quarantine by default.** Every deletion moves items to the quarantine store. Irreversible
   deletion requires `--purge` **and** the user typing the literal `PURGE`. `--purge` ignores `--yes`.
3. **Protected volumes are read-only.** Never write to volumes with role `system`, `preboot`,
   `recovery`, or `vm`. No flag can enable this.
4. **Explicit confirmation.** No deletion without user confirmation. No TTY and no `--yes` → exit code `7`.
5. **`cloud-synced` is never deleted.** Broza only reports and shows the provider's official steps.
6. **stdout is data, stderr is conversation.** Prompts, progress, warnings, and the donation message go to stderr.
7. **Honest numbers.** Purgeable space is never summed into free space. Quarantined bytes are
   reported separately from reclaimed bytes.

Structural enforcement: every mutating function requires an `Approved<_>` token that only
`crates/broza/src/safety/guard.rs` can construct. `grep -rn "Approved<" crates/` is the audit surface.

## 3. Closed decisions

Do not reopen without an explicit user instruction in the current conversation.

- PRD §11 decisions `D1`–`D15` (monetization, Rust + MIT, Apple Silicon only, two latest macOS
  majors, APFS + HFS+, name, English everywhere, `diskutil -plist` enumeration for v1, quarantine
  semantics and `quarantine` command, RF-10 deferred, treemap deferred).
- Gap resolutions `G1`–`G15` recorded in `docs/cli-spec.md` §8 and `docs/prd.md`.
- If a requirement seems wrong, write an ADR proposal in `docs/adr/` and ask. Never deviate silently.

## 4. Architecture map

Cargo workspace. Paths below are the planned layout; keep new code inside it.

```
crates/broza/            core library, crates.io name `broza`, no clap / no terminal / no env access
  src/model/             serde types = the JSON contract (no I/O)
  src/ports/             traits only: ProcessRunner, DiskEnumerator, SpaceProvider, SnapshotProvider,
                         FileOps, Clock, Prompter; `Ports` DI bundle
  src/adapters/          the ONLY place for `unsafe`, `std::process::Command`, objc2, libc
  src/scan/              parallel walker (dua-core), aggregation, cache, mount table
  src/detect/            Detector trait, Registry, filters, exclusions, explain text, detectors/<category>.rs
  src/safety/            guard (Approved token), confirmation policy, role check, path canonicalization, ExitCode
  src/clean/             planner (dry-run shape), executor, actions
  src/quarantine/        store, manifest, mover, restore, expiry
  src/config/            schema, layering (defaults < toml < profile < env < flags), keys
  src/error.rs           one `BrozaError` enum, maps 1:1 to ExitCode
crates/broza-cli/        binary `broza`: clap, rendering (human / json / csv), TTY prompter, donation gate
```

Layer rules:

- `model` has no I/O and no platform code. Changing it changes the JSON contract (see §8).
- `ports` contains traits only. `adapters` implement them. Nothing outside `adapters` spawns
  processes, calls objc2, or uses `unsafe`.
- `safety` is the only module that constructs `Approved<_>`. `clean`, `quarantine`, and
  `SnapshotProvider::delete` take `&Approved<_>` for every write.
- `broza-cli` never imports `adapters` for mutation; it wires `Ports` and renders `Envelope<T>`.
- One detector per file under `detect/detectors/`. Detectors never delete; they return `Finding`s.

## 5. Commands

```bash
cargo build --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo llvm-cov --workspace --all-features --fail-under-lines 80
cargo insta review
```

Run all of them before reporting work as done. Report actual output, including failures.

## 6. Coding conventions

- Rust 2024 edition. `#![deny(unsafe_code)]` in every crate root; `#[allow(unsafe_code)]` only in `adapters/`.
- No `unwrap` / `expect` in library code. `thiserror` in core, `anyhow` only in the binary.
- Immutable by default: functions return new values; no in-place mutation of shared state.
- Files 200–400 lines (800 hard max). Functions under 50 lines. Split before growing.
- Named constants for thresholds, TTLs, sizes. No magic numbers.
- Naming: `snake_case` for JSON fields, `kebab-case` for category ids and config keys, `PascalCase` types.
- All sizes are `u64` bytes. All timestamps are RFC 3339 UTC strings in JSON. Formatting is presentation-layer only (`broza-cli/src/output/bytes.rs`).
- Early returns over nesting (max 4 levels).
- Errors are handled explicitly at every layer; detector failures become `warnings[]`, never a crash.

## 7. Testing rules

- TDD: write the failing test first, run it, implement, run, refactor.
- Unit tests live beside the code; integration tests in each crate's `tests/`.
- No test touches the real `$HOME`, real disks, or spawns `diskutil`/`tmutil`. The one exception is
  an `#[ignore]` benchmark, which may read `$HOME` read-only and never runs in CI. Use `tempfile` plus
  the fakes: `FakeRunner` (fixture plists in `crates/broza/tests/fixtures/plist/<macos_major>/`),
  `FakeFileOps`, `FakePrompter`, `FixedClock`.
- `insta` snapshots for every JSON example in `docs/cli-spec.md` §4 and for every `--help` output.
- `assert_cmd` tests for every exit code `0`–`9`.
- Confirmation policy has an exhaustive truth-table test.
- A `trybuild` compile-fail test proves deletion functions cannot be called without `Approved<_>`.
- Coverage gate: 80% lines, enforced in CI.

## 8. JSON contract rules

- `schema_version` follows semver. Minor bump: additive optional fields only. Major bump: rename or remove.
- Enums are `#[non_exhaustive]`; consumers must ignore unknown fields and values.
- Any change to `crates/broza/src/model/` requires updating `docs/cli-spec.md` §4 and its snapshot tests in the same PR.
- Breaking changes require an ADR and a major bump.

## 9. Git workflow

- Conventional commits: `feat|fix|refactor|docs|test|chore|perf|ci: <description>`.
- One milestone per branch (`m0-skeleton`, `m1-safety-kernel`, ...). Small PRs inside a milestone are fine.
- PR body: summary, test plan, and the invariants touched (or "none").
- CI green (build, clippy, fmt, tests, coverage) before requesting review.

## 10. Language

Code, comments, commit messages, documentation, and all CLI text are in English.

## 11. Workflow hints for agents

- Before editing `safety/`, `clean/`, or `quarantine/`: re-read §2 and the relevant ADR, then state the plan.
- Prefer small edits over rewrites. Do not reformat unrelated code.
- Spec changes: edit `docs/cli-spec.md`, bump its version, add an ADR, update snapshots. Never leave code and spec disagreeing.
- Research before writing new utility code: check crates.io and the reuse list in `docs/implementation-plan.md` §2.
- When a macOS behavior is uncertain (see `docs/implementation-plan.md` §9), write the check as a test with a fixture, not as an assumption in code.
- The guides under `site/explain/` copy prose verbatim from `crates/broza/src/detect/category_text.rs`, `crates/broza/src/adapters/diskutil/purpose.rs` and the README. When one of those paragraphs changes, update the matching page and its `<lastmod>` in `site/sitemap.xml`.
