# ADR 0007 — Snapshot deletion names one UUID on one volume

Status: Accepted (2026-09-22)

## Context

`docs/cli-spec.md` §3.4 promises that a `clean` plan item acts on exactly what it names.
For an APFS local snapshot the natural tool is `tmutil deletelocalsnapshots <date>`, and the
first M4 design (implementation plan, decision 2) used it. Its date form, however, deletes
the snapshot with that timestamp on **every** eligible APFS volume: Time Machine creates
local snapshots on all such volumes in one pass, so same-second timestamps across volumes are
common. A plan item that named one volume would then delete on others too — including a
volume the `snapshots` detector never listed — and the confirmation the user gave would not
describe what happened.

`diskutil apfs deleteSnapshot <volume> -uuid <uuid>` removes one snapshot on one volume
(`diskutil` calls the UUID form "preferred"); `diskutil apfs listSnapshots -plist` already
reports every snapshot's UUID.

## Decision

- Deletion runs `diskutil apfs deleteSnapshot <volume> -uuid <uuid>`. `tmutil` is not used to
  delete anything.
- A snapshot is actionable only with a UUID, a volume and a mount point; the detector leaves
  out any snapshot without a UUID, and the plan item (`items[].snapshot`) carries all three.
- The safety kernel checks a snapshot item against the finding's own list (name, UUID,
  volume, purgeable, Time Machine prefix), the mount table (volume id and mount point agree,
  role is data or user) and requires `size_bytes: 0`; it touches no path.
- The contract keeps the action name `tmutil_delete`: it is a stable enum value of schema 1.x.
- The `backup` role accepts no action at all; local snapshots live on data and user volumes.

## Consequences

- A user who confirms "delete these N snapshots" gets exactly those N, on the volumes shown.
- A snapshot macOS reports without a UUID cannot be deleted by Broza; it is still counted and
  shown in `suggest`.
- If `diskutil` refuses for lack of ownership or privileges, the item fails with
  `permission_denied` and the `snapshot_needs_admin` warning carries the exact command to run
  with `sudo`; Broza never escalates (§6).
