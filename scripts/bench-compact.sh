#!/usr/bin/env bash
#
# What compacting a large live value does to the copying collector, measured.
# (The generational one copies it once whatever you do; see the comments in
# benches/Compact.)
#
#   scripts/bench-compact.sh
#
# Runs `benches/Compact` three times, each part in its own process so the
# collector's report covers that part alone: the benchmarks beside a
# 200_000-entry map left in the heap, the same beside the map compacted, and
# the cost of compacting it. Each run prints its table and then what the
# collector did -- `meadow run --gc-stats`.
#
# The two numbers to compare are "copied by collections" and "collecting":
# with the map in the heap every collection copies it again, compacted it is
# copied once. The work each benchmark does is otherwise identical.

set -euo pipefail
cd "$(dirname "$0")/.."

meadow=buildtools/target/release/meadow
[ -x "$meadow" ] || cargo build --release --quiet --manifest-path buildtools/Cargo.toml -p meadow

bold=$(tput bold 2>/dev/null || true)
plain=$(tput sgr0 2>/dev/null || true)

for part in plain compact cost; do
  case $part in
    plain) title="beside the map, left in the heap" ;;
    compact) title="beside the same map, compacted" ;;
    cost) title="compacting it" ;;
  esac
  echo "${bold}== $title${plain}"
  # The report goes to stderr, after the table.
  MEADOW_BENCH_COMPACT=$part "$meadow" run --gc copying --gc-stats benches/Compact 2>&1 \
    | sed -n '/^benchmark /,$p' | grep -v '^=>'
  echo
done
