# ADR 0009 — APFS clone families are counted once, by clone id

Status: Accepted (2026-09-23)

## Context

`st_blocks` reports the blocks a file references, not the blocks it owns. An APFS clone
(`clonefile(2)`; `cp -c`, the Finder's Duplicate, and the save path of many apps) references
the same blocks as its original, so a walk that sums `st_blocks` counts a family once per
member. On the developer's Mac, `~/Library/Group Containers/group.net.whatsapp.WhatsApp.shared/Message/Media`
holds 1.09 million files of which a sampled 23,844 had 263 distinct contents: `broza scan`
listed it at 434.6 GB on a volume with 392 GB in use, and `du` agreed with the wrong number.
`size_exceeds_volume` warned, as 1.0.0 promised (PRD RF-02: "best-effort, completed post-1.0"),
but a consumers list led by a figure larger than the disk is a list nobody can act on.

APFS exposes the family through `ATTR_CMNEXT_CLONEID`, an extended common attribute of
`getattrlist(2)` and `getattrlistbulk(2)` requested with `FSOPT_ATTR_CMN_EXTENDED`. Measured on
macOS 26 and pinned by `crates/broza/tests/fakes_behave_like_std.rs`: a file that was never
cloned reports its own inode; every clone reports the original's inode; a hard link shares the
inode and so the id; the bit is `0x100` (the `0x40` some sources quote is `ATTR_CMNEXT_REALDEVID`),
which closes `docs/implementation-plan.md` §9 question 3. The original and an ordinary file
therefore read alike, and only the clones announce the family.

## Decision

1. `EntryMetadata` and `FileEntry` carry `clone_id: Option<u64>`, regular files only. The bulk
   reader asks for it in the fork slot of its request; the plain adapter asks `getattrlist` for
   the one attribute after `lstat`, so the bulk reader's cross-check still compares equals.
   `None` means the filesystem has no such notion, never "not a clone".
2. The walk keeps a ledger, not a record per clone: per family, the clone whose path sorts first
   and what it counted, and the directories holding one; per directory, what its clones counted.
   It also remembers the `(device, inode)` of every file that could be an original — inodes only,
   sorted at the end. A record per clone with its path was tried first and cost 1.2 GB on the
   developer's disk (1.8 million clones); the ledger costs a quarter of that.
3. After hard links are settled, each family is settled: when the original was seen, it keeps the
   bytes as an ordinary file and every clone is discounted; when it was not, the clone whose path
   sorts first keeps them. Discounted clones leave the totals and the largest-item figures, stay in
   the file lists with `allocated_bytes` zero (removing one frees nothing, and the detectors need
   to see the family). A clone that also has several names is settled as a hard link only: its
   names settle to one, counted where it stands.
4. A directory holding clones stays cacheable. Marking it uncacheable, as a hard link's
   directories are, re-walked the whole media folder on every warm run (warm `suggest` went from
   13 s to 17 s). Instead each cache record names the families whose credited clone is directly
   inside it, and a subtree served from the cache hands those families back as if their originals
   had been seen, so any other clone the warm walk meets is discounted as the cold walk discounted
   it. Cache records also carry each file's clone id (store layout 4; older stores are replaced),
   so a subtree served from the cache gives the detectors the same files a walk would.
   `duplicates` proposes neither a clone nor a file that has one among the families the walk
   knows of (`WalkResult::clone_families`: the clones met, the families served subtrees keep,
   and the clones in the file lists); `large-old-files` proposes no file whose removal frees
   nothing; `trash`, `user-cache` and `build-cache` size a file the way the walk settled it.
   The file report keeps shared-bytes files (hard links, clones) in a heap of their own, so a
   folder of a million clones cannot push real files out of the top-N before settlement.
6. What `clean --purge` counts as reclaimed follows the same rule as a hard link: a clone
   frees nothing certain, so it counts for nothing (`reclaimed_bytes` is never more than what
   was freed, AGENTS.md §2.7). Whether a clone's family is all gone is not knowable at purge
   time; the figure stays what is certain.
5. The walk runs on its own thread pool with 64 MiB stacks. The recursion is one frame per
   directory level, a macOS path allows 512 levels, and the ledger made the frames large enough
   to overflow the default 2 MiB at 466 levels on a test tree. Reserved, not committed.

## Consequences

- WhatsApp's media folder measures what deleting it would free. `du -sh` still does not.
- A clone written to after cloning owns the blocks it changed and is still discounted whole: the
  clone id says "family", not "how much is shared". Such a directory can measure less than it
  holds. Partial-clone accounting (`fcntl F_LOG2PHYS_EXT` per extent) is the remaining deferred
  item in `docs/cli-spec.md` §8.2.
- Purging the *original* of a family that still has clones reports its blocks as freed: the
  executor sees a plain file, and the family is not known at purge time. `reclaimed_bytes`
  overstates by that file in that one case.
- The records say where a family's credited clone is, not where its original is: a family whose
  original sits in a cached subtree while one of its clones is walked counts once per side, and a
  family whose original was deleted after its clones' directories were recorded stays discounted
  until their records expire. Both err towards a larger figure, or towards no change, and are
  bounded by `cache-ttl`; `size_exceeds_volume` still fires when the sum exceeds the volume.
- One extra syscall per regular file on the plain-adapter path (rare: the bulk reader carries
  the id in the same call). Measured on the developer's Data volume (4.8 million entries, 1.8
  million clones, 2.5 million possible originals): a cold `scan` takes 37–44 s and 1.4 GB peak
  against 31–39 s and 0.75–0.89 GB for 1.0.0 on the same runs; a warm `suggest` takes 8 s, as
  before. The families and the originals are what the extra memory is; a family's first path
  could be kept as a directory reference and a name to halve it, if it ever matters.
