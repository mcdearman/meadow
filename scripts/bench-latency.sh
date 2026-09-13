#!/usr/bin/env bash
#
# How long each collector stops a program that keeps a lot alive, measured.
#
#   scripts/bench-latency.sh
#   MEADOW_BENCH_ENTRIES=2000000 scripts/bench-latency.sh    -- a bigger map
#
# Runs `benches/latency` -- a large map kept alive while small requests update
# it -- once with the copying collector and once with the generational one, on
# one worker thread so the two see the same machine. Each run prints the slowest
# request it served and then what the collector did: `meadow run --gc-stats`.
#
# The lines to compare are "slowest" and "pauses". The copying collector's
# pauses grow with the map; the generational one's should not.

set -euo pipefail
cd "$(dirname "$0")/.."

meadow=buildtools/target/release/meadow
[ -x "$meadow" ] || cargo build --release --quiet --manifest-path buildtools/Cargo.toml -p meadow

bold=$(tput bold 2>/dev/null || true)
plain=$(tput sgr0 2>/dev/null || true)

for gc in copying generational; do
  echo "${bold}== $gc${plain}"
  MEADOW_THREADS=1 "$meadow" run --gc "$gc" --gc-stats benches/latency 2>&1 \
    | sed -n '/^built /,$p' | grep -v '^=>'
  echo
done
