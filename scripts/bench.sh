#!/usr/bin/env bash
#
# Run the benchmarks on both runtimes and compare them.
#
#   scripts/bench.sh                 # the `benches` package
#   scripts/bench.sh examples/euler  # anything with a `main` that runs benchmarks
#
# This is what Criterion's saved baseline does for a single program, pointed at
# the question Meadow actually has: the bytecode VM and the CEK machine are two
# implementations of one language, and the CEK is the specification. A ratio
# below 1 means the VM is slower than the interpreter it replaced, which is worth
# knowing immediately.
#
# Numbers from a laptop are a rumour. Close the other windows, run it twice, and
# distrust any row whose spread is a large fraction of its median.

set -euo pipefail
cd "$(dirname "$0")/.."

pkg="${1:-benches}"
meadow=buildtools/target/release/meadow
[ -x "$meadow" ] || cargo build --release --quiet --manifest-path buildtools/Cargo.toml -p meadow

bold=$(tput bold 2>/dev/null || true)
plain=$(tput sgr0 2>/dev/null || true)

# Just the table: from the header to the end, minus the program's own result.
run() { "$meadow" run ${2:-} "$pkg" 2>/dev/null | sed -n '/^benchmark /,$p' | grep -v '^=>'; }

echo "${bold}== bytecode VM${plain}"
vm=$(run vm)
echo "$vm"
echo
echo "${bold}== CEK machine${plain}"
cek=$(run cek --cek)
echo "$cek"
echo
echo "${bold}== throughput, VM relative to CEK${plain}"
# The table is fixed-width and benchmark names contain spaces, so the columns are
# taken by position rather than by splitting. Throughput is `1_234_567/s`, so the
# grouping and the unit come off.
join_on_name() {
  awk 'NR > 2 && NF {
    name = substr($0, 1, 22); gsub(/ +$/, "", name)
    t = substr($0, 59, 16); gsub(/[_]|\/s| /, "", t)
    print name "\t" t
  }'
}
paste <(echo "$vm" | join_on_name) <(echo "$cek" | join_on_name) |
  awk -F'\t' '
    $1 == $3 && $4 + 0 > 0 {
      printf "  %-22s %10.2fx   (%s vs %s per second)\n", $1, $2 / $4, $2, $4
      next
    }
    { printf "  %-22s %10s\n", $1, "not in both runs" }'
