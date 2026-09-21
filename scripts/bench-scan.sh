#!/usr/bin/env bash
#
# bench-scan.sh — measure a scan against the budget of docs/cli-spec.md §7.
#
#   | scenario                        | target   |
#   |---------------------------------|----------|
#   | scan of the boot disk, cold     | < 10 s   |
#   | scan with a warm cache          | <  1 s   |
#   | first visible result on screen  | < 500 ms |
#
# This script is documentation you can run, not CI: it reads the real home
# directory and its numbers depend on the machine, so no pipeline may gate on it.
#
# Usage:
#   scripts/bench-scan.sh                  # walk the home directory, cold then warm
#   BENCH_ROOT=/path scripts/bench-scan.sh # walk something else
#   BENCH_EXCLUDE=a:b scripts/bench-scan.sh
#
# Cloud folders are excluded by default. Dropbox, iCloud Drive and Google Drive
# are network filesystems: `lstat` on a file that is not downloaded blocks on the
# provider, and a walk of a home directory holding them measures the network
# rather than the walker (a first run here spent 14 minutes at 0% CPU). Broza
# itself still walks them — reporting cloud-synced folders is the point of
# RF-09 — but a throughput benchmark must not.
#
# While `broza scan` is not wired to the scanner yet (M2 part A owns the CLI),
# the benchmark runs the ignored integration test `bench_home_walk`, which walks
# the tree through the same `StdFileOps` port and the same cache store the CLI
# will use. When the command exists, add the two `hyperfine` lines at the bottom.

set -euo pipefail

cd "$(dirname "$0")/.."

export PATH="/opt/homebrew/opt/rustup/bin:${HOME}/.cargo/bin:${PATH}"

REAL_HOME="${HOME}"
BENCH_ROOT="${BENCH_ROOT:-${HOME}}"
export BENCH_ROOT

if [[ -z "${BENCH_EXCLUDE:-}" ]]; then
  # Anchored to the home directory, not to what is being walked: the cloud
  # providers put their roots there whatever subtree the benchmark is aimed at.
  home="${REAL_HOME}"
  cloud=(
    "${home}/Library/CloudStorage"      # OneDrive, Box, the new Dropbox
    "${home}/Library/Mobile Documents"  # iCloud Drive
    "${home}/Dropbox"
  )
  while IFS= read -r folder; do
    [[ -n "${folder}" ]] && cloud+=("${folder}")
  done < <(find "${home}" -maxdepth 1 -name '*Drive*' -type d 2>/dev/null || true)
  BENCH_EXCLUDE="$(
    IFS=:
    echo "${cloud[*]}"
  )"
fi
export BENCH_EXCLUDE

echo "==> building the release profile"
cargo build --release --workspace --all-features

echo "==> walking ${BENCH_ROOT} (cold, then warm from a cache in a temporary directory)"
echo "    excluding: ${BENCH_EXCLUDE}"
cargo test --release --all-features --test scan_bench -- --ignored --nocapture

# Once `broza scan` is wired to scan_all (M2 part A):
#
#   hyperfine --warmup 0 --runs 3 './target/release/broza scan --no-cache --json'
#   hyperfine --warmup 1 --runs 5 './target/release/broza scan --json'
#
# The first is the cold number (< 10 s), the second the warm one (< 1 s).
