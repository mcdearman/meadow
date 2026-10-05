#!/usr/bin/env bash
#
# Has this commit's full suite already passed on Linux somewhere else?
#
#   scripts/checked-elsewhere.sh OWNER/REPO SHA
#
# The suite is run on a rented machine before a commit is pushed --
# `scripts/check-parallel.sh`, which is `check.sh` and the release's smoke test
# -- and whoever ran it says so on the commit, as a status called
# `suite/linux-x86_64`. A workflow that finds that status has nothing to add by
# running the same checks on the same kind of machine for another forty
# minutes, and skips them. Windows is checked nowhere else, and never skipped.
#
# Prints `skip=true` or `skip=false`, for `$GITHUB_OUTPUT`. The status is
# posted just after the push that starts the workflow, so it is looked for
# for two minutes before it is taken not to be coming.
#
# To say a commit passed:
#
#   gh api repos/OWNER/REPO/statuses/SHA -f state=success \
#     -f context=suite/linux-x86_64 -f description="check-parallel.sh passed"

set -uo pipefail

repo="${1:?usage: checked-elsewhere.sh OWNER/REPO SHA}"
sha="${2:?usage: checked-elsewhere.sh OWNER/REPO SHA}"

for _ in $(seq 1 12); do
  state=$(gh api "repos/$repo/commits/$sha/statuses" \
    --jq '[.[] | select(.context == "suite/linux-x86_64")][0].state // ""' 2>/dev/null || true)
  if [ "$state" = "success" ]; then
    echo "skip=true"
    exit 0
  fi
  sleep 10
done
echo "skip=false"
