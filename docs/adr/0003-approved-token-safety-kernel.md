# ADR 0003 — Safety kernel with an `Approved<_>` capability token

- Status: Accepted
- Date: 2026-09-21

## Context

PRD §7.3 invariants (dry-run default, quarantine, protected volumes, explicit confirmation,
cloud-synced never deleted) must survive refactors, new detectors, and contributor PRs.
Policy checks scattered across call sites are easy to forget or bypass.

## Decision

- A `safety` module owns a `Guard` whose `approve(plan, request, mounts, config)` returns
  `Result<Approved<Write>, GuardRejection>`. `Approved<_>` contains a private unit-struct seal,
  so it can only be constructed inside `safety::guard`.
- Every mutating function (`clean::actions::*`, `quarantine::{mover,restore,expiry}`,
  `SnapshotProvider::delete`) takes `&Approved<_>` as a parameter. There is no other write path.
- `approve` performs, in order: `--apply` present; canonicalize without following symlinks;
  resolve volume via a firmlink-aware mount table; reject roles `system|preboot|recovery|vm`;
  enforce root allowlist; apply exclusions; enforce `--max-size`; reject any `inform_only` item;
  compute confirmation mode.
- `confirmation_policy(apply, max_risk, purge, yes, tty, ci) -> ConfirmationMode` is a pure
  function with an exhaustive truth-table test.
- `Finding::new` forces `action = inform_only, actionable = false` for `cloud-synced`.
- A single `ExitCode` enum (0–9) is the only thing `main` maps to `process::exit`.

## Consequences

- `grep "Approved<"` enumerates every write path; a `trybuild` compile-fail test proves a
  deletion without the token does not compile.
- Adding a detector cannot introduce a new deletion path: detectors only return findings.
- Slight friction: tests of executors must build an `Approved` through the guard with fakes.
