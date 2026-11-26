# ADR 0011 — `broza doctor`, and a Full Disk Access check that words the permission hint

Status: Accepted (2026-11-26)

## Context

Granting Full Disk Access was the least friendly part of Broza. A scan on a Mac without it
printed "421 locations could not be read … Grant Full Disk Access", and two things were wrong
with that:

- Above five refusals the walk collapses them into one summary, and the System Settings hint
  matched single refusals only, so the hint never printed in the common case.
- Root-owned system folders (`.Spotlight-V100`, `.fseventsd`, `.DocumentRevisions-V100`) are
  refused **with** Full Disk Access too. A Mac that already had it was still told to grant it,
  and could not tell whether granting it had worked.

macOS offers no public call that answers "does this process have Full Disk Access". The
established technique is to list a folder that only such a process may list.

## Decision

- `broza::access::check_full_disk_access` lists the first existing of
  `~/Library/Application Support/com.apple.TCC`, `~/Library/Safari`, `~/Library/Mail`,
  `~/Library/Messages`: listed → `granted`, refused → `missing`, otherwise `unknown`. The folders
  are under the home and owned by the user, so a refusal there can only be the privacy control.
  It goes through the `FileOps` port, so the fakes test every outcome.
- A new read-only command, `broza doctor`, reports the verdict, the folder it rests on, the
  terminal application to grant access to (from `TERM_PROGRAM`), the numbered steps including
  "quit and reopen the application", and where the quarantine store and the scan cache live.
  `--open` runs `open x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles`
  through the process port. It always exits `0`: the state is data.
- The stderr hint after a run that met a refusal is worded by the same check, run once and
  only then: off → `broza doctor --open`; on → nothing to do; unknown → `broza doctor`.

## Alternatives considered

- **Tell refusals apart by errno** (`EPERM` for the privacy control, `EACCES` for ownership).
  It would need the errno carried through the walker's diagnostics into the warning model, and
  macOS does not document that the split holds on every path. The probe answers the question
  the user has, once, at no cost to the walk. Left in the backlog.
- **A first-run prompt** offering to open System Settings from `scan`. A read-only command
  that prompts is surprising, and the prompt cannot verify the result; `doctor` can.

## Consequences

- One more command in the CLI surface and the JSON contract (`DoctorReport`, schema 1.2,
  additive). `AccessState` is a closed enum: only Broza produces it.
- The check costs one `stat` per candidate and one directory listing, only on runs with a
  refusal and on `doctor`.
- The settings link is Apple's legacy pane identifier, which current macOS still honours. If a
  release drops it, `--open` fails with a `settings_not_opened` warning and the printed steps
  still apply.
