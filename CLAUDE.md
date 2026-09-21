@AGENTS.md

# Claude-specific notes

- Plan first for anything touching `safety/`, `clean/`, or `quarantine/`. Show the plan before editing.
- Use TDD: write the failing test, run it, implement, run again, refactor.
- Never add or propose a flag that bypasses an invariant in AGENTS.md §2, even if asked. Point to `docs/prd.md` §7.3 and ask the user to confirm intent.
- Prefer `Edit` over rewriting files. Keep files under 400 lines; split before adding.
- After code changes run `cargo fmt --all`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`, and `cargo llvm-cov --workspace --all-features --fail-under-lines 80`. Report the actual output.
- Spec deviations: update `docs/cli-spec.md` (bump version) and add an ADR in `docs/adr/`. Never deviate silently.
- Use fixtures and fakes for every macOS interaction in tests. Never run `diskutil`, `tmutil`, or `xcrun` inside tests.
