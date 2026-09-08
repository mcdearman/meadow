#!/usr/bin/env bash
#
# Build `meadow-<version>.vsix`.
#
# Needs node and npm. The extension bundles `vscode-languageclient`, so
# `node_modules` has to be present and is packaged into the artifact — hence
# `npm install` rather than `npm ci --omit=dev`.

set -euo pipefail
cd "$(dirname "$0")"

npm install --silent
npx --yes @vscode/vsce package --allow-missing-repository --skip-license

echo
ls -1 ./*.vsix
