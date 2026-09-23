# ADR 0010 — The walk lists directories by descriptor, and reaches deep ones by relative steps

Status: Accepted (2026-09-23)

## Context

macOS refuses any path of `PATH_MAX` (1024) bytes or more with `ENAMETOOLONG`, in every
path-taking call: `open`, `lstat`, `getattrlist`, `realpath`. The limit applies to what the kernel
resolves, so a symlink on the way (`/var` → `/private/var`) can push a shorter string over it. A
directory tree can still be *built* deeper than that, because `mkdir` with a short relative name
never sees the full path; programs that descend with relative names (test generators, some package
managers and build tools) do it, the Finder cannot. Broza's walk opened and stated every entry by
full path, so on such a tree it stopped at the 1024-byte mark, reported one `unreadable_entry`
warning per blocked entry with a kilobyte-long path, and counted nothing below. The developer's
own deep-tree probe in a scratch directory showed exactly that during the M5 verification runs.

`getattrlistbulk(2)`, which the walk already uses for every directory listing, works on a
descriptor. The missing pieces were how a deep directory is opened and how the entries the bulk
reader leaves out (directories, entries with a per-entry error) are stated.

A first version kept the parent's descriptor open while the children were walked and opened each
child with `openat(parent_fd, name)`. Review measured it: descriptors in use went from a handful
to threads × depth (1 409 on a 10 × 150 tree), and at exhaustion whole subtrees were lost behind
four warnings. That design is not the one recorded here.

## Decision

1. `FileOps` gains `open_dir(path) -> DirHandle` and `list_dir(handle, path)`, with path-based
   defaults the in-memory fake keeps (its handle is the path; its `open_dir` refuses what a real
   `open(O_DIRECTORY)` would refuse, so the walker's error arm is exercised). `StdFileOps` makes
   the handle a descriptor and lists through it: `getattrlistbulk` on the descriptor, entries the
   buffer leaves out stated with `fstatat(dir_fd, name, AT_SYMLINK_NOFOLLOW)` and `getattrlistat`
   for the clone id, and — when the bulk reader will not answer — `readdir` on a duplicate of the
   descriptor plus `fstatat` per entry. Nothing in the listing hands the kernel a path
   (`adapters/dir_fd.rs`, `adapters/std_fs_dirs.rs`).
2. Each directory is opened on its own, by path, with `O_DIRECTORY | O_NOFOLLOW | O_NONBLOCK |
   O_CLOEXEC`. When the kernel answers `ENAMETOOLONG`, the deepest ancestor that does open is found
   by asking the kernel, ancestor by ancestor, and the rest of the way is opened one relative name
   at a time through the previous descriptor, closing it. The depth costs calls — a few hundred
   short `open`s for a directory a few hundred levels past the limit — never descriptors.
3. The walker opens a directory, lists it, and drops the handle before descending into the
   children (`walker.rs walk_dir`). The walk therefore holds one descriptor per directory being
   listed, as the path-based walk did; `tests/deep_paths.rs` passes under `ulimit -n 256`. No
   descriptor limit is raised, and the library has no process-global side effect.
4. The plain adapter's `read_dir_with_metadata(path)`, used by the detectors on shallow paths, is
   unchanged.

## Consequences

- A tree deeper than a path can name is measured to the bottom (`tests/deep_paths.rs`, 520
  levels on the real filesystem; the same leaf is unreachable by `lstat`). The bulk reader's
  cross-check compares against `fstatat`; `tests/fakes_behave_like_std.rs` pins `open_dir` +
  `list_dir` against `read_dir_with_metadata` on both the fake and the real filesystem.
- Items below the limit are scan-only. The safety kernel canonicalizes every plan item with
  `realpath`, and rename, remove and quarantine moves are path calls; all fail at 1024 bytes,
  kernel side, so such an item is refused at `clean` with the kernel's error. Correct, and rare.
- One `open` per directory replaces one `open` per directory: no extra syscalls on an ordinary
  tree; the descriptor's identity is not re-checked against the listing that named it, as before.
- The clone id has one reader for both the path and the descriptor form (`adapters/clone_id.rs`),
  so the two cannot drift and refuse the bulk reader for the process.
