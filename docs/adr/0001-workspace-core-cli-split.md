# ADR 0001 — Cargo workspace: core library `broza` + binary crate `broza-cli`

- Status: Accepted
- Date: 2026-09-21

## Context

Phase 1 ships a CLI. Phase 2 ships a proprietary SwiftUI GUI that must reuse the same engine
through FFI (UniFFI or swift-bridge). The PRD reserves the crates.io name `broza` for the core.

## Decision

One Cargo workspace with two crates:

- `crates/broza`: core library, MIT, published as `broza`. Contains domain model (serde types),
  ports (traits), adapters (macOS), scan, detectors, safety kernel, clean, quarantine, config.
  No clap, no terminal output, no direct environment access.
- `crates/broza-cli`: binary named `broza`. Argument parsing, rendering (human/JSON/CSV),
  TTY prompting, donation gate. Wires concrete adapters into the core's `Ports` bundle.

Public core API stays FFI-friendly: `Engine::new(Ports)` plus functions that return `model`
types or JSON strings. No generic trait objects in the public surface.

## Consequences

- The GUI consumes `broza` without linking clap or terminal code.
- The JSON contract lives in `crates/broza/src/model/` and is the single boundary for both UIs.
- Slightly more boilerplate (two `Cargo.toml`, DI wiring) than a single crate.
