# ADR 0006 — The scan performance budget is a rate, not a wall-clock target

**Status:** Accepted
**Date:** 2026-09-21
**Deciders:** M2 scanner work (`crates/broza/src/scan/`, `crates/broza/src/adapters/bulk_dir/`)

## Context

`docs/cli-spec.md` §7 has said since draft 1.0 that a cold `scan` of the boot
disk finishes in under ten seconds. That number was written before anything had
been measured, and it silently assumes a disk size — ten seconds for *what*?

The M2 walker has now been measured against a real home directory
(`scripts/bench-scan.sh`, Apple Silicon, APFS, cloud-provider roots excluded
because they block on the network rather than on the disk):

| run | entries | wall clock | rate |
|---|---|---|---|
| cold, `readdir` + `lstat` per entry | 3 651 311 | 42.3 s | ≈ 86 000 entries/s |
| cold, one `getattrlistbulk` per directory | 3 668 920 | 23.7 s | ≈ 155 000 entries/s |
| cold, same build, second machine state | 3 668 544 | 26.5 s | ≈ 138 000 entries/s |
| cold, as measured in review | ≈ 3.7 M | ≈ 29 s | ≈ 127 000 entries/s |
| cold, final, three runs | 3 706 320 | 28.3 s / 29.2 s / 29.3 s | ≈ 127 000 entries/s |
| warm, same three runs | 3 706 330 | 5.7 s / 3.6 s / 3.9 s | 17 653 of 270 448 directories walked |

**The store is not where the warm time goes.** 15.5 MB holding 270 302 records
takes 0.09 s to encode and write and 0.07 s to read back and decode. An earlier
report of a 2.9 s warm scan blamed that decode; timing the two separately
disproved it. Indexing the store or sharding it per top-level directory would
buy about a tenth of a second and is not planned.

**The warm budget of one second is not met on a tree this size, and the cold
budget holds with about a quarter of its margin.** A warm scan walks the 6.5%
of directories the cache may not answer for: everything at or above `--depth`,
every subtree holding an item at least `--min-size` (so the largest consumers
are always re-measured, which is the point), every subtree with an unreadable
directory below it, and every subtree that holds some but not all names of a
hard-linked file. Those exclusions are what make a warm scan report exactly
what a cold one reports; they are not tuning knobs. Which of them dominates on
a given machine has not been attributed — the next measurement worth taking is
a count of walked directories by reason.

That home directory is 648 GB and 3.7 million entries: several times the size
of the "512 GB Data volume" the milestone had in mind, and no amount of
optimisation makes 3.7 million entries fit into ten seconds on this hardware —
each one costs at least one `stat`-equivalent from the filesystem.

## Decision

State the cold budget as a **throughput**: at least 100 000 entries per second,
which is ten seconds for a one-million-entry Data volume — the size the
original target was written for. The warm and first-result budgets stay
absolute, because they are about what the user waits for and not about how much
disk there is: warm under one second, first result under 500 ms.

`crates/broza/tests/scan_bench.rs` asserts the rate, so the budget is checked
against whatever tree the person running it has, rather than against an
assumption about disk size.

## Consequences

- The spec's §7 table now carries the rate alongside the ten-second figure, and
  says which volume size that figure assumes.
- A machine that gets slower — a regression in the walker, a filesystem change
  in a new macOS — fails the benchmark on any tree, not only on a big one.
- The benchmark stays `#[ignore]`d and out of CI: it reads a real home
  directory, and its numbers depend on hardware (`AGENTS.md` §7).
- If Apple ever exposes a cheaper bulk interface, or if Broza learns to skip
  whole subtrees from volume metadata, the rate is the number to revisit — not
  the ten seconds.
- A store is only rewritten when a record was added, changed or expired: a warm
  scan that measures the same numbers again leaves the file alone, which also
  means a reused measurement expires when the TTL says rather than being
  renewed by every scan that read it.
- A hard link only stops a subtree from being cached when one of its names is
  outside that subtree. A directory holding every name of a file is
  self-contained and may be served: skipping it still counts that file once,
  because nobody outside will count it.
- The warm budget of `docs/cli-spec.md` §7 stays absolute (< 1 s) and is
  currently missed on a 3.7 M-entry home. Restating it as a rate, the way the
  cold budget was restated, is the obvious next question — but not before
  somebody has attributed the walked directories to their reasons, because the
  answer may be that a cheaper rule exists rather than a bigger number.
