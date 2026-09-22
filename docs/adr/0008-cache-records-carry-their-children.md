# ADR 0008 — Cache records carry their children, so a subtree is served whole

Status: Accepted (2026-09-22)

## Context

`docs/cli-spec.md` §7 budgets a full `suggest` at under 15 s. Until M4 `suggest` walked the
home cold on every run (36 s on the developer's home, kernel time in `getattrlistbulk`),
because a cache record held one directory's aggregate and nothing else: a subtree served from
the cache was one node, and the detectors need every directory under the home (`node_modules`
three levels into a project, `__pycache__` beside a module) and, since the `duplicates` and
`large-old-files` detectors, every file of at least 1 MB. Serving a subtree would have hidden
candidates; not serving cost the whole walk.

The first idea in the plan — records that remember whether a subtree holds any name a detector
looks for — would have tied the cache format to the detector list and still could not carry
files.

## Decision

- A record (`DirRecord`, store layout version 3) carries, besides the aggregate: the names and
  `(inode, mtime)` of the directories directly inside it, and every file directly inside it of at
  least `CACHE_FILE_FLOOR_BYTES` (1 MB, equal to the detectors' file floor) with its name, sizes,
  inode, link count and times. Names are stored as bytes, since a path need not be UTF-8.
- The store serves a subtree by rebuilding it record by record (`CacheStore::subtree`): the
  directory asked about, then every descendant through the child lists, each with its files.
  A subtree is served whole or not at all: a missing, expired or unusable record anywhere below
  (hard link to a name outside, unreadable hole) means the subtree is walked.
- The walker collects the files above the cache floor on every cached walk (`cache_files`,
  unbounded) and `records_of` writes them; the report's own bounded file list is unchanged.
- The reuse rule of §7 becomes: serve when the request's floor is at least the cache floor, or
  when nothing inside reaches the request's floor. A warm scan still reports exactly what a cold
  one would, up to the TTL-bounded staleness the cache always had.
- `suggest` sets its `--min-size` to the file report's floor and reads the cache. `clean` does
  not read it: its plan is checked item by item against the disk (§3.4), and a size remembered
  from before a file grew would be refused there and abort the run. Both refresh the store.
- A store file in an older layout is discarded and rewritten; only a version from the future or a
  corrupt file is exit `9`.

## Consequences

- `suggest` warm on the developer's home: 7.6–8.7 s, with the same findings as the cold run.
- The store grows to hold names and big files: about 30 MB for a home of a few million entries.
- What `suggest` shows may be up to `cache-ttl` old for files that grew in place, as `scan`
  always could; `--no-cache` walks cold. `clean` stays at the cold rate.
- New with this layout: a directory whose mtime was set back after its contents changed (`rsync -t`,
  `tar -p`, a restore) is served with the names its record kept, so a warm report can list
  entries that are gone or miss new ones until the TTL. Layout 1 could only report a stale size.
- A refused subtree is remembered for the walk (`Denied`), so a change deep in a tree costs one
  key-only descent, not one per ancestor the walker asks about; a subtree is materialised only
  after it passed.
- `clean` walks with `serve_from_cache: false`: the store is loaded and refreshed, never replaced
  by the home walk alone.
- Still open: one cloud placeholder file makes every directory above it unservable
  (`has_truncation`), which predates this layout; splitting "counted placeholder" from "hole"
  would let those subtrees be served.
- `FileEntry` carries identity and times from the walk, so detectors never re-`stat` a file and
  reading one for comparison cannot change the answer.
