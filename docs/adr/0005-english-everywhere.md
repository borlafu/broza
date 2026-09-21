# ADR 0005 — English everywhere

- Status: Accepted
- Date: 2026-09-21

## Context

The draft PRD and CLI spec were written in Spanish, and the spec's example outputs and JSON
text fields (`purpose`, `description`, `reasoning`) were Spanish with comma decimals. RNF-07
states the CLI is in English. The Phase 1 audience is the global open-source developer community.

## Decision

- CLI output, error messages, JSON text fields, code, comments, commit messages, documentation,
  README, ADRs, and agent guidance are written in English.
- Numbers use a decimal point and decimal units (`138.2 GB`, `1.00 TB`), matching Finder.
- The Spanish drafts are replaced by `docs/prd.md` (v1.0) and `docs/cli-spec.md` (v1.1); the
  drafts are removed from the repository.
- GUI localization (ES/EN) remains a Phase 2 concern (RNF-07).

## Consequences

- One language for contributors and agents; no translation drift between spec and code.
- The product name and its Spanish origin are explained once in the README.
