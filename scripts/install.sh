#!/bin/sh
# Meadow installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/mcdearman/meadow/master/scripts/install.sh | sh
#
# Builds the whole toolchain from source and installs it: `meadowup`, which
# looks after which version of Meadow you have, and `meadow`, the build system.
# That needs a Rust toolchain and takes a few minutes. Pass --from-release to
# download prebuilt binaries instead, which is quick but only as new as the last
# release.
#
# Installing is meadowup's job either way -- this script builds, then hands the
# binaries over -- so there is one implementation of where things go and how
# your PATH is edited, rather than two that can come to disagree.
#
# Working on Meadow itself? `--local .` builds this checkout and installs it
# the way a release would, and `--with-extension` builds and installs the VS
# Code extension alongside it -- so what you test is how it would be installed.
#
# Run with --help for the options.

set -eu

REPO="mcdearman/meadow"
MEADOW_HOME="${MEADOW_HOME:-$HOME/.meadow}"
BIN_DIR="$MEADOW_HOME/bin"

VERSION="latest"
# Source is the default: a release is only as new as the last one cut, and
# someone running this from a checkout wants what is in front of them.
FROM_RELEASE=0
LOCAL=""
UNINSTALL=0
EXTENSION=0
# Passed through to meadowup, which is what actually edits the PATH.
PASS_THROUGH=""

main() {
    parse_args "$@"

    # Uninstalling needs meadowup and nothing else, so do not build a toolchain
    # only to delete it.
    if [ "$UNINSTALL" -eq 1 ]; then
        [ -x "$BIN_DIR/meadowup" ] || err "meadow is not installed at $MEADOW_HOME"
        exec "$BIN_DIR/meadowup" uninstall $PASS_THROUGH
    fi

    if [ "$FROM_RELEASE" -eq 1 ]; then
        target="$(detect_target)"
        if fetch_meadowup "$target"; then
            if [ "$VERSION" = "latest" ]; then
                exec "$BIN_DIR/meadowup" install $PASS_THROUGH
            else
                exec "$BIN_DIR/meadowup" install --version "$VERSION" $PASS_THROUGH
            fi
        fi
        say "no prebuilt meadowup for $target — building from source"
    fi

    build_everything
}

# --- argument parsing -------------------------------------------------------

parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --version)
                [ $# -ge 2 ] || err "--version needs a tag, e.g. --version v0.1.0-alpha"
                VERSION="$2"
                shift 2
                ;;
            --from-release) FROM_RELEASE=1; shift ;;
            # Kept because it was the flag that meant this, and it still does:
            # building from source is now what happens anyway.
            --from-source) FROM_RELEASE=0; shift ;;
            --local)
                [ $# -ge 2 ] || err "--local needs the path to a meadow checkout"
                LOCAL="$2"
                FROM_RELEASE=0
                shift 2
                ;;
            --no-modify-path) PASS_THROUGH="$PASS_THROUGH --no-modify-path"; shift ;;
            --force) PASS_THROUGH="$PASS_THROUGH --force"; shift ;;
            --with-extension) EXTENSION=1; shift ;;
            --uninstall) UNINSTALL=1; shift ;;
            --help|-h) usage; exit 0 ;;
            *) err "unknown option: $1 (try --help)" ;;
        esac
    done
}

usage() {
    cat <<'EOF'
Builds the Meadow toolchain from source and installs it.

    scripts/install.sh [options]

    --from-release     download prebuilt binaries instead of building
    --version <TAG>    that tag, rather than the newest
    --local <PATH>     build from a checkout already on disk
    --with-extension   also build and install the VS Code extension
    --no-modify-path   do not touch your shell profiles
    --force            install again even if it is already here
    --uninstall        remove Meadow and undo the PATH entry

Building needs a Rust toolchain and takes a few minutes; --from-release is
quick, but only as new as the last release.

Afterwards, `meadowup` does this job on its own:

    meadowup update       bring the toolchain up to date
    meadowup show         what is installed, and where
    meadowup uninstall

MEADOW_HOME overrides where everything goes.
EOF
}

# --- getting meadowup -------------------------------------------------------

# Download meadowup for $1. Returns non-zero (without exiting) when there is no
# such asset, so the caller can fall back to building it.
fetch_meadowup() {
    target="$1"
    asset="meadowup-${target}"
    if [ "$VERSION" = "latest" ]; then
        url="https://github.com/$REPO/releases/latest/download/$asset"
    else
        url="https://github.com/$REPO/releases/download/$VERSION/$asset"
    fi

    say "downloading $url"
    tmp="$(mktemp -d)"
    # shellcheck disable=SC2064
    trap "rm -rf \"$tmp\"" EXIT

    if ! download "$url" "$tmp/meadowup"; then
        rm -rf "$tmp"
        trap - EXIT
        return 1
    fi

    mkdir -p "$BIN_DIR"
    chmod 755 "$tmp/meadowup"
    replace_binary "$tmp/meadowup" meadowup

    rm -rf "$tmp"
    trap - EXIT
}

# Build the whole toolchain and install it.
#
# Both binaries are built, staged in one directory, and handed to the meadowup
# that was just built -- so installing them, and editing the PATH, is the same
# code that runs when they are downloaded instead.
build_everything() {
    need_cmd cargo
    src="$(toolchain_source)"

    say "building meadowup"
    cargo build --release --manifest-path "$src/installer/Cargo.toml" --bin meadowup \
        || err "could not build meadowup"

    say "building the toolchain (this takes a few minutes)"
    cargo build --release --manifest-path "$src/buildtools/Cargo.toml" -p meadow \
        || err "could not build meadow"

    staging="$(mktemp -d)"
    # shellcheck disable=SC2064
    trap "rm -rf \"$staging\"" EXIT
    cp "$src/installer/target/release/meadowup" "$staging/meadowup"
    cp "$src/buildtools/target/release/meadow" "$staging/meadow"

    # An older meadowup than this script would not know `--from`, which happens
    # when the two come from different commits. Say that, rather than letting it
    # fail with an option it has never heard of.
    built="$src/installer/target/release/meadowup"
    if ! "$built" help 2>/dev/null | grep -q -- "--from"; then
        err "the meadowup built from $src is older than this script and cannot be
       handed binaries to install. Run it against a matching checkout:
       scripts/install.sh --local <path-to-this-checkout>"
    fi

    "$built" install --from "$staging" $PASS_THROUGH \
        || err "could not install what was built"

    rm -rf "$staging"
    trap - EXIT

    [ "$EXTENSION" -eq 1 ] && install_extension "$src"
    return 0
}

# Build the VS Code extension from `$1` and install it, for someone working on
# Meadow who wants the editor to follow what they just built.
install_extension() {
    src="$1"
    [ -x "$src/editors/vscode/build.sh" ] \
        || err "$src has no editors/vscode/build.sh"
    say "building the VS Code extension"
    (cd "$src" && ./editors/vscode/build.sh) || err "could not build the extension"

    vsix="$(ls -1 "$src"/editors/vscode/*.vsix 2>/dev/null | head -1)"
    [ -n "$vsix" ] || err "the extension built but produced no .vsix"

    if command -v code >/dev/null 2>&1; then
        say "installing $(basename "$vsix")"
        # `--force` because the version usually has not changed between two
        # local builds, and without it VS Code declines to reinstall it.
        code --install-extension "$vsix" --force >/dev/null \
            || err "could not install the extension"
        say ""
        say "Reload the editor window (Developer: Reload Window) so the language"
        say "server restarts on the binary just built."
    else
        say ""
        say "\`code\` is not on PATH, so the extension was built but not installed:"
        say "  code --install-extension $vsix --force"
    fi
}

# Where to build from: a checkout named with --local, or one fetched into
# $MEADOW_HOME/src. Fetching into the same place each time means a second run is
# an incremental build rather than a clean one.
toolchain_source() {
    if [ -n "$LOCAL" ]; then
        [ -f "$LOCAL/installer/Cargo.toml" ] \
            || err "$LOCAL does not look like a meadow checkout (no installer/)"
        echo "$LOCAL"
        return
    fi

    # Run from inside a checkout, build that checkout: "from source" should mean
    # the source in front of you, not a fresh clone of master. It also keeps this
    # script and the meadowup it builds at the same version, which they must be
    # -- one passes options the other has to understand.
    #
    # `[ -f "$0" ]` is what tells a checkout apart from `curl … | sh`, where the
    # script is not a file on disk at all and the directory above it means
    # nothing.
    if [ "$VERSION" = "latest" ] && [ -f "$0" ]; then
        here="$(cd "$(dirname "$0")/.." 2>/dev/null && pwd)" || here=""
        if [ -n "$here" ] && [ -f "$here/installer/Cargo.toml" ] \
            && [ -f "$here/buildtools/Cargo.toml" ]; then
            say "building this checkout: $here" >&2
            echo "$here"
            return
        fi
    fi

    need_cmd git
    src="$MEADOW_HOME/src"
    say "fetching source into $src" >&2
    if [ -d "$src/.git" ]; then
        git -C "$src" fetch --depth 1 origin >/dev/null 2>&1 || err "git fetch failed"
        git -C "$src" reset --hard FETCH_HEAD >/dev/null 2>&1 || err "git reset failed"
    else
        rm -rf "$src"
        if [ "$VERSION" = "latest" ]; then
            git clone --depth 1 "https://github.com/$REPO.git" "$src" >/dev/null 2>&1 \
                || err "git clone failed"
        else
            git clone --depth 1 --branch "$VERSION" "https://github.com/$REPO.git" "$src" \
                >/dev/null 2>&1 || err "git clone failed (no such tag: $VERSION?)"
        fi
    fi
    echo "$src"
}


# Put `$1` at $BIN_DIR/$2, replacing whatever is there.
#
# The old binary is renamed out of the way first: writing over an executable that
# is currently running fails on Linux (ETXTBSY), while renaming it never does, so
# an upgrade works even with one open elsewhere. Unlinking a busy file is fine on
# Unix, so the leftover goes immediately.
replace_binary() {
    dest="$BIN_DIR/$2"
    if [ -f "$dest" ]; then
        rm -f "$dest.old"
        mv "$dest" "$dest.old" 2>/dev/null || true
    fi
    install -m 755 "$1" "$dest" 2>/dev/null \
        || { cp "$1" "$dest" && chmod 755 "$dest"; }
    rm -f "$dest.old"
}

# --- helpers ----------------------------------------------------------------

detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Darwin) os_part="apple-darwin" ;;
        Linux)  os_part="unknown-linux-gnu" ;;
        *) err "unsupported OS: $os (try --from-source)" ;;
    esac

    case "$arch" in
        x86_64|amd64)  arch_part="x86_64" ;;
        arm64|aarch64) arch_part="aarch64" ;;
        *) err "unsupported architecture: $arch (try --from-source)" ;;
    esac

    echo "${arch_part}-${os_part}"
}

download() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$1" -O "$2"
    else
        err "need curl or wget"
    fi
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || err "need $1"
}

say() {
    echo "$1"
}

err() {
    echo "error: $1" >&2
    exit 1
}

main "$@"
