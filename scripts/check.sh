#!/usr/bin/env bash
#
# Every check, in one place. CI runs *this script*, not its own copy of the
# steps — so a green run here means a green run there, and adding a check means
# editing one file.
#
#   scripts/check.sh                 tests, and the checks that pass today
#   scripts/check.sh --strict        ...plus rustfmt and clippy (not clean yet)
#   scripts/check.sh --version v0.1.0    ...plus: the tag matches the manifests
#
# Runs from anywhere; paths are resolved against the repository root.

set -euo pipefail

cd "$(dirname "$0")/.."

STRICT=0
VERSION=""
while [ $# -gt 0 ]; do
  case "$1" in
    --strict) STRICT=1 ;;
    --version) VERSION="${2:-}"; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

# The tree is five independent Cargo workspaces, so there is no one
# `cargo test --workspace` that covers it. Missing one is the whole reason this
# script exists.
WORKSPACES="compiler eval rts buildtools installer"

step() { printf '\n\033[1m== %s\033[0m\n' "$1"; }

# --- version -----------------------------------------------------------------
#
# Release assets carry no version, so nothing downstream would notice a tag that
# disagrees with the manifests. Check it while we still can.

if [ -n "$VERSION" ]; then
  step "version"
  want="${VERSION#v}"
  for ws in $WORKSPACES; do
    got=$(grep -m1 '^version = ' "$ws/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
    if [ "$got" != "$want" ]; then
      echo "  $ws/Cargo.toml says $got, tag says $want" >&2
      exit 1
    fi
    echo "  $ws $got"
  done
fi

# --- tests -------------------------------------------------------------------

for ws in $WORKSPACES; do
  step "test: $ws"
  (cd "$ws" && cargo test --quiet)
done

# --- the language itself -----------------------------------------------------
#
# `cargo test` in `buildtools` already runs the standard library's own tests
# through the library API. Running them again through the CLI is what proves the
# `meadow test` command works, which nothing else covers.

step "meadow test --std"
cargo run --quiet --manifest-path buildtools/Cargo.toml -p meadow -- test --std

step "meadow fmt --check"
cargo run --quiet --manifest-path buildtools/Cargo.toml -p meadow -- fmt --check lib/Std

# --- strict ------------------------------------------------------------------
#
# Not yet gating: rustfmt disagrees with the tree in ~220 places (mostly
# edition-2024 import ordering) and clippy has ~42 warnings. Both are worth
# fixing, and once they are, move these above and drop the flag.

if [ "$STRICT" = "1" ]; then
  for ws in $WORKSPACES; do
    step "fmt: $ws"
    (cd "$ws" && cargo fmt --check)
    step "clippy: $ws"
    (cd "$ws" && cargo clippy --all-targets -- -D warnings)
  done
fi

printf '\n\033[1;32mall checks passed\033[0m\n'
