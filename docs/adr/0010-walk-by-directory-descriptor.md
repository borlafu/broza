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
   for the clone id, and — when the bulk reader will not answer — `readdir` on a fresh descriptor
   of the same directory (`openat(fd, ".")`: a duplicate shares the position the bulk reader
   moved to the end, and rewinding it does not move what `readdir` sees, so it lists nothing;
   measured and pinned by a test) plus `fstatat` per entry; a `readdir` error is an error, never a
   shorter directory. Reopening `.` needs search permission on the directory, which every
   `fstatat` below would need as well, so a directory without it is one warning instead of one
   per entry. Nothing in the listing hands the kernel a path
   (`adapters/dir_fd.rs`, `adapters/std_fs_dirs.rs`).
2. Each directory is opened on its own, by path, with `O_DIRECTORY | O_NOFOLLOW | O_NONBLOCK |
   O_CLOEXEC`. When the kernel answers `ENAMETOOLONG`, the deepest ancestor that does open is found
   by asking the kernel, ancestor by ancestor (strings of 1024 bytes or more are skipped without
   a call), and the rest of the way is opened relative to it: one `openat` with `O_NOFOLLOW_ANY`
   per stretch of the relative path shorter than `PATH_MAX`, so thirty-two names cost one call, no
   symlink is followed anywhere, and the depth costs calls, never descriptors.
3. The walker opens a directory, lists it, and drops the handle before descending into the
   children (`walker.rs walk_dir`), so on an ordinary tree the walk holds one descriptor per
   directory being listed, as the path-based walk did — plus the anchors of item 4 on a deep one,
   and, in fallback mode only, whatever a thread waiting on the parallel `fstatat`s picks up.
   `tests/deep_paths.rs` passes under `ulimit -n 256`, in CI too. No descriptor limit is raised,
   and the library has no process-global side effect.
4. Reaching each deep directory from the top would cost steps quadratic in the depth past the
   limit (a review measured 35 s for 8 chains of 600 levels, 289 s for 64). So once a path is 768
   bytes long, the walker keeps one handle open every 32 levels as the anchor the directories
   below are opened from (`FileOps::open_dir_below`, `walker/anchor.rs`): at most 32 names per
   directory, opened in one call, one extra descriptor per 32 levels on each chain being walked,
   none on an ordinary tree.
5. After opening, the descriptor's `(device, inode)` is compared with the listing that named the
   directory (`FileOps::dir_identity`, `fstat`). Opening by path follows symlinks in every
   component but the last, so an ancestor swapped for a link between the listing and the open
   would otherwise be walked under the wrong identity — and cached under it. A mismatch is a
   warning and the directory is left unwalked. On a filesystem whose inode numbers are not
   stable across a vnode reclaim (some FUSE and network filesystems, outside the supported APFS
   and HFS+) that would drop the subtree with the same warning.
6. The plain adapter's `read_dir_with_metadata(path)`, used by the detectors on shallow paths, is
   unchanged. The in-memory fake's `open_dir` refuses what `open(O_DIRECTORY | O_NOFOLLOW)`
   refuses — a symlink to a directory included — so the two agree in the contract test.

## Consequences

- A tree deeper than a path can name is measured to the bottom (`tests/deep_paths.rs`, 520
  levels on the real filesystem; the same leaf is unreachable by `lstat`). The bulk reader's
  cross-check compares against `fstatat`; `tests/fakes_behave_like_std.rs` pins `open_dir` +
  `list_dir` against `read_dir_with_metadata` on both the fake and the real filesystem.
- Items below the limit are scan-only. The safety kernel canonicalizes every plan item with
  `realpath`, and rename, remove and quarantine moves are path calls; all fail at 1024 bytes,
  kernel side, so such an item is refused at `clean` with the kernel's error. Correct, and rare.
- One `open` and one `fstat` per directory replace one `open`: no other extra syscalls on an
  ordinary tree.
- The clone id has one reader for both the path and the descriptor form (`adapters/clone_id.rs`),
  so the two cannot drift and refuse the bulk reader for the process.
