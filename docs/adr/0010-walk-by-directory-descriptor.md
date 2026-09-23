# ADR 0010 — The walk descends by directory descriptor, not by path

Status: Accepted (2026-09-23)

## Context

macOS refuses any path of `PATH_MAX` (1024) bytes or more with `ENAMETOOLONG`, in every
path-taking call: `open`, `lstat`, `getattrlist`, `realpath`. A directory tree can still be
*built* deeper than that, because `mkdir` with a short relative name never sees the full path;
programs that descend with relative names (test generators, some package managers and build
tools) do it, the Finder cannot. Broza's walk opened and stated every entry by full path, so on
such a tree it stopped at the 1024-byte mark, reported one `unreadable_entry` warning per blocked
entry with a kilobyte-long path, and counted nothing below. The developer's own deep-tree probe
in a scratch directory showed exactly that during the M5 verification runs.

`getattrlistbulk(2)`, which the walk already uses for every directory listing, works on a
descriptor. The missing pieces were how each directory is opened and how the entries the bulk
reader leaves out (directories, entries with a per-entry error) are stated.

## Decision

1. `FileOps` gains three methods with path-based defaults: `open_dir(path)`, `open_dir_in(parent,
   name, path)` and `list_dir(dir, path)`, over an opaque `DirHandle`. The in-memory fake keeps
   the defaults (its handle is the path). `StdFileOps` overrides them: the handle is a descriptor,
   `open_dir_in` is `openat(parent_fd, name, O_DIRECTORY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)`,
   `list_dir` is `getattrlistbulk` on the descriptor, and entries the buffer leaves out are stated
   with `fstatat(dir_fd, name, AT_SYMLINK_NOFOLLOW)` and `getattrlistat` for the clone id
   (`adapters/dir_fd.rs`). The bulk reader's cross-check compares against `fstatat` too.
2. The walker opens each child from its parent's handle by name (`walker.rs walk_dir`), so no call
   sees a full path. Paths are still assembled in memory for the report, the cache keys and the
   findings; user space has no `PATH_MAX`.
3. The soft limit on open descriptors is raised once per process to the hard limit, capped at
   `OPEN_MAX` (10 240) as the kernel demands. A walk holds one descriptor per level of the chain
   each thread is descending; a terminal's default soft limit is 256 on some setups. A descriptor
   that cannot be opened is a warning for that directory, as before.
4. The fallback when the bulk reader is refused or unsupported (`readdir` + `lstat` by path) keeps
   the old limit: it is the path the kernel gives on filesystems without `getattrlistbulk`, and it
   stops where paths stop.

## Consequences

- A tree deeper than a path can name is measured to the bottom (`tests/deep_paths.rs`, 520
  levels on the real filesystem; the same leaf is unreachable by `lstat`).
- Items below the limit are scan-only. The safety kernel canonicalizes every plan item with
  `realpath`, and rename, remove and quarantine moves are path calls; all fail at 1024 bytes,
  kernel side, so such an item is refused at `clean` with the kernel's error. Correct, and rare.
- One `openat` per directory replaces one `open` per directory: no extra syscalls. The plain
  adapter's `read_dir_with_metadata` is unchanged for the callers outside the walk.
