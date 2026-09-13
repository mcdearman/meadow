#!/usr/bin/env bash
#
# Install everything a release installs, built from this checkout.
#
#   scripts/install-local.sh                  meadow, and the VS Code extension
#   scripts/install-local.sh --no-extension   just meadow
#   scripts/install-local.sh --no-modify-path leave PATH alone
#
# A release ships three things: `meadow`, the installer that puts it on your
# PATH, and the `.vsix`. This builds all three from the working tree and
# installs them *through the same code the release uses* — `meadow-setup` on
# Windows, `install.sh` elsewhere — so what you test is how it would be
# installed, not a copy of a copy. The point is iteration: change something,
# run this, reload the editor.
#
# Builds are incremental: the binaries come from the ordinary `target/` dirs,
# not a fresh clone.
#
# MEADOW_HOME is honoured, as it is by both installers, so
#   MEADOW_HOME=/tmp/m scripts/install-local.sh --no-modify-path --no-extension
# installs somewhere disposable.

set -euo pipefail
cd "$(dirname "$0")/.."
repo="$(pwd)"

EXTENSION=1
MODIFY_PATH=1
for arg in "$@"; do
  case "$arg" in
    --no-extension) EXTENSION=0 ;;
    --no-modify-path) MODIFY_PATH=0 ;;
    -h|--help)
      sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown option: $arg (try --help)" >&2; exit 2 ;;
  esac
done

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) windows=1 ;;
  *) windows=0 ;;
esac

# --- meadow ----------------------------------------------------------------------

if [ "$windows" -eq 1 ]; then
  step "building meadow and meadow-setup"
  cargo build --release --manifest-path buildtools/Cargo.toml -p meadow
  cargo build --release --manifest-path installer/Cargo.toml

  step "installing with meadow-setup"
  # `--from` makes the installer take this binary instead of downloading one;
  # everything else — where it goes, moving a locked `meadow.exe` aside while an
  # editor's language server holds it, the PATH entry — is the release's own code.
  setup_args=(--from "$repo/buildtools/target/release/meadow.exe" --no-pause)
  [ "$MODIFY_PATH" -eq 0 ] && setup_args+=(--no-modify-path)
  [ -n "${MEADOW_HOME:-}" ] && setup_args+=(--dir "$MEADOW_HOME")
  installer/target/release/meadow-setup.exe "${setup_args[@]}"
else
  step "installing with install.sh"
  install_args=(--local "$repo")
  [ "$MODIFY_PATH" -eq 0 ] && install_args+=(--no-modify-path)
  sh ./install.sh "${install_args[@]}"
fi

# --- the VS Code extension -------------------------------------------------------

if [ "$EXTENSION" -eq 1 ]; then
  step "building the VS Code extension"
  editors/vscode/build.sh
  vsix="$(ls -1 editors/vscode/*.vsix | head -1)"

  if command -v code >/dev/null 2>&1; then
    step "installing $(basename "$vsix")"
    # `--force` because the version usually has not changed between two local
    # builds, and without it VS Code declines to reinstall the same version.
    code --install-extension "$vsix" --force
  else
    echo
    echo "\`code\` is not on PATH, so the extension was built but not installed:"
    echo "  code --install-extension $vsix --force"
  fi
fi

bin_dir="${MEADOW_HOME:-$HOME/.meadow}/bin"
step "done"
echo "  $("$bin_dir/meadow" --version 2>/dev/null || echo meadow) in $bin_dir"
echo
echo "Reload the editor window (Developer: Reload Window) so the language server"
echo "restarts on the new binary. It extracts the matching Std sources on start."
