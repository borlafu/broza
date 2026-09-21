#!/usr/bin/env bash
#
# Record the `diskutil` / `tmutil` output Broza parses, redacted so it can live in
# the repository as a test fixture (`docs/implementation-plan.md` §3.6).
#
# Usage:
#   scripts/capture-diskutil-fixtures.sh [macos_major]
#
#   macos_major   Major version the fixtures belong to. Defaults to the running
#                 system, so on macOS 26 the files land in
#                 crates/broza/tests/fixtures/plist/macos26/.
#
# Every command below is read-only. Nothing is mounted, unmounted, repaired or
# deleted; the script never runs `diskutil` with a verb that writes.
#
# What is captured
#   list.plist                        diskutil list -plist
#   apfs_list.plist                   diskutil apfs list -plist
#   apfs_list_snapshots_data.plist    diskutil apfs listSnapshots -plist /System/Volumes/Data
#   apfs_list_snapshots_system.plist  diskutil apfs listSnapshots -plist /
#   info_<dev>.plist                  diskutil info -plist <whole disk>, one per physical disk
#   info_root.plist                   diskutil info -plist /
#   info_data.plist                   diskutil info -plist /System/Volumes/Data
#   info_preboot.plist                diskutil info -plist /System/Volumes/Preboot
#   tmutil_listlocalsnapshots.txt     tmutil listlocalsnapshots /
#
# What is redacted (all files are redacted in one pass, so a UUID keeps the same
# fake value everywhere it appears)
#   * UUIDs             replaced by deterministic fakes. Distinct UUIDs get
#                       distinct fakes, so relationships such as "this physical
#                       store is that container" survive the redaction.
#   * serial numbers    the value of any key whose name contains "Serial".
#   * user identity     the short user name, the full name and the computer name,
#                       wherever they appear (including inside volume names and
#                       mount points such as /Users/<you>).
#   * temp directories  the per-user hash in /private/var/folders/<a>/<b>/.
#
# What is deliberately kept
#   * every size, capacity and byte count, so the fixtures exercise real numbers.
#   * volume names macOS assigns itself (Macintosh HD, Data, Preboot, Recovery,
#     VM, Update, xART, Hardware, iSCPreboot, EFI) and names of mounted disk
#     images, which describe software rather than a person.
#   * roles, mount points, filesystem types and flags: they are the contract
#     Broza parses.
#
# Review the result before committing it: the redaction list above is the known
# set of identifying fields, not a guarantee about a machine nobody has seen.

set -euo pipefail

readonly REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly FIXTURE_ROOT="${REPO_ROOT}/crates/broza/tests/fixtures/plist"
readonly DISKUTIL="/usr/sbin/diskutil"
readonly TMUTIL="/usr/bin/tmutil"
readonly PLUTIL="/usr/bin/plutil"
# Mount points whose `diskutil info` output is recorded under a stable name.
readonly NAMED_TARGETS=(
  "root:/"
  "data:/System/Volumes/Data"
  "preboot:/System/Volumes/Preboot"
)
# Volumes whose snapshots are recorded. The Data volume usually has none and the
# System volume carries the `com.apple.os.update-*` ones, so both shapes — an
# empty list and a populated one — end up in the fixtures.
readonly SNAPSHOT_TARGETS=(
  "data:/System/Volumes/Data"
  "system:/"
)

main() {
  if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//; $d'
    exit 0
  fi
  require_tools

  local major="${1:-$(sw_vers -productVersion | cut -d. -f1)}"
  local out_dir="${FIXTURE_ROOT}/macos${major}"
  # Global, not local: the EXIT trap fires after this function has returned.
  RAW_DIR="$(mktemp -d)"
  local raw_dir="${RAW_DIR}"
  trap 'rm -rf "${RAW_DIR:-}"' EXIT

  capture_all "${raw_dir}"
  redact_all "${raw_dir}"
  lint_all "${raw_dir}"

  mkdir -p "${out_dir}"
  cp "${raw_dir}"/* "${out_dir}/"
  echo "wrote $(ls -1 "${raw_dir}" | wc -l | tr -d ' ') fixtures to ${out_dir}"
}

# Fail early and by name when a tool this script needs is missing.
require_tools() {
  local tool
  for tool in "${DISKUTIL}" "${TMUTIL}" "${PLUTIL}" /usr/bin/perl; do
    [[ -x "${tool}" ]] || die "required tool not found: ${tool}"
  done
}

die() {
  echo "capture-diskutil-fixtures: $*" >&2
  exit 1
}

# Run every read-only command, writing raw output into "${1}".
capture_all() {
  local raw_dir="$1"

  "${DISKUTIL}" list -plist >"${raw_dir}/list.plist"
  "${DISKUTIL}" apfs list -plist >"${raw_dir}/apfs_list.plist"

  local disk
  for disk in $(whole_disks "${raw_dir}/list.plist"); do
    "${DISKUTIL}" info -plist "${disk}" >"${raw_dir}/info_${disk}.plist"
  done

  local target
  for target in "${NAMED_TARGETS[@]}"; do
    capture_one "${raw_dir}/info_${target%%:*}.plist" info -plist "${target#*:}"
  done
  for target in "${SNAPSHOT_TARGETS[@]}"; do
    capture_one "${raw_dir}/apfs_list_snapshots_${target%%:*}.plist" \
      apfs listSnapshots -plist "${target#*:}"
  done

  "${TMUTIL}" listlocalsnapshots / >"${raw_dir}/tmutil_listlocalsnapshots.txt"
}

# Write `diskutil <args…>` into "${1}", skipping a target this machine does not
# have rather than failing the whole capture.
capture_one() {
  local destination="$1"
  shift
  if "${DISKUTIL}" "$@" >"${destination}" 2>/dev/null; then
    return 0
  fi
  rm -f "${destination}"
  echo "skipped \`diskutil $*\`: unavailable on this machine" >&2
}

# Print the BSD name of every whole disk listed in the plist "${1}", one per line.
whole_disks() {
  "${PLUTIL}" -extract WholeDisks json -o - "$1" | tr -d '[]"' | tr ',' '\n'
}

# Rewrite every captured file in place, in one pass so the UUID map is shared.
redact_all() {
  local raw_dir="$1"
  local short_name full_name computer_name
  short_name="$(id -un)"
  full_name="$(id -F 2>/dev/null || echo "${short_name}")"
  computer_name="$(scutil --get ComputerName 2>/dev/null || hostname -s)"

  BROZA_SHORT_NAME="${short_name}" \
  BROZA_FULL_NAME="${full_name}" \
  BROZA_COMPUTER_NAME="${computer_name}" \
    /usr/bin/perl "${REPO_ROOT}/scripts/redact-diskutil-fixtures.pl" "${raw_dir}"/*
}

# Reject a fixture the plist parser of macOS itself will not accept.
lint_all() {
  local file
  for file in "$1"/*.plist; do
    "${PLUTIL}" -lint "${file}" >/dev/null || die "invalid plist after redaction: ${file}"
  done
}

main "$@"
