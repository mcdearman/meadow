#!/usr/bin/env bash
#
# Build `meadow-<version>.vsix`.
#
# Needs node and npm. The extension bundles `vscode-languageclient`, so
# `node_modules` has to be present and is packaged into the artifact — hence
# `npm install` rather than `npm ci --omit=dev`.
#
# `vsce` is a pinned devDependency and is run from `node_modules`, not through
# `npx @vscode/vsce`. `npx` fetches the *latest* version, and vsce 3 raised its
# floor to Node 20 — so the unpinned form silently stopped working on Node 18
# with a `ReferenceError: File is not defined` from deep inside a transitive
# dependency, which is not a message anyone can act on. Pinning makes the build
# reproducible and keeps it working on the Node the repository actually targets.

set -euo pipefail
cd "$(dirname "$0")"

npm install --silent

# `vsce` prunes devDependencies from the package itself, so pinning it here does
# not put it in the artifact.
node node_modules/@vscode/vsce/vsce package --allow-missing-repository --skip-license

echo
ls -1 ./*.vsix
