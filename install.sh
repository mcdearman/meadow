#!/bin/sh
# Meadow installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/mcdearman/meadow/master/install.sh | sh
#
# This script does one thing: get `meadowup` onto the machine. Everything after
# that is meadowup's job -- it installs itself into ~/.meadow/bin, fetches the
# toolchain, and edits your PATH. That is why this file is short, and why the
# same program does the work on Windows, where there is no shell script at all.
#
# Run with --help for the options.

set -eu

REPO="mcdearman/meadow"
MEADOW_HOME="${MEADOW_HOME:-$HOME/.meadow}"
BIN_DIR="$MEADOW_HOME/bin"

VERSION="latest"
FROM_SOURCE=0
LOCAL=""
UNINSTALL=0
# Passed through to meadowup, which is what actually edits the PATH.
PASS_THROUGH=""

main() {
    parse_args "$@"

    if [ "$FROM_SOURCE" -eq 1 ]; then
        build_meadowup
    else
        target="$(detect_target)"
        if ! fetch_meadowup "$target"; then
            say "no prebuilt meadowup for $target — building from source"
            build_meadowup
        fi
    fi

    # From here it is meadowup's. `uninstall` is handed over too, so there is
    # one implementation of it rather than this script's and meadowup's
    # disagreeing about what to remove.
    if [ "$UNINSTALL" -eq 1 ]; then
        exec "$BIN_DIR/meadowup" uninstall $PASS_THROUGH
    fi
    if [ "$VERSION" = "latest" ]; then
        exec "$BIN_DIR/meadowup" install $PASS_THROUGH
    else
        exec "$BIN_DIR/meadowup" install --version "$VERSION" $PASS_THROUGH
    fi
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
            --from-source) FROM_SOURCE=1; shift ;;
            --local)
                [ $# -ge 2 ] || err "--local needs the path to a meadow checkout"
                LOCAL="$2"
                FROM_SOURCE=1
                shift 2
                ;;
            --no-modify-path) PASS_THROUGH="$PASS_THROUGH --no-modify-path"; shift ;;
            --force) PASS_THROUGH="$PASS_THROUGH --force"; shift ;;
            --uninstall) UNINSTALL=1; shift ;;
            --help|-h) usage; exit 0 ;;
            *) err "unknown option: $1 (try --help)" ;;
        esac
    done
}

usage() {
    cat <<'EOF'
Installs meadowup, which installs the rest of the Meadow toolchain.

    install.sh [options]

    --version <TAG>    a particular release, not the newest
    --from-source      build meadowup with cargo instead of downloading it
    --local <PATH>     build from a checkout already on disk
    --no-modify-path   do not touch your shell profiles
    --force            install again even if it is already here
    --uninstall        remove Meadow and undo the PATH entry

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

build_meadowup() {
    need_cmd cargo
    mkdir -p "$BIN_DIR"

    # A checkout already on disk: build it where it is, so a second run is an
    # incremental build rather than a clean one.
    if [ -n "$LOCAL" ]; then
        [ -f "$LOCAL/installer/Cargo.toml" ] \
            || err "$LOCAL does not look like a meadow checkout (no installer/)"
        say "building meadowup from $LOCAL"
        cargo build --release --manifest-path "$LOCAL/installer/Cargo.toml" --bin meadowup \
            || err "cargo build failed"
        replace_binary "$LOCAL/installer/target/release/meadowup" meadowup
        return
    fi

    need_cmd git
    src="$MEADOW_HOME/src"
    say "fetching source into $src"
    if [ -d "$src/.git" ]; then
        git -C "$src" fetch --depth 1 origin >/dev/null 2>&1 || err "git fetch failed"
        git -C "$src" reset --hard FETCH_HEAD >/dev/null 2>&1 || err "git reset failed"
    else
        rm -rf "$src"
        if [ "$VERSION" = "latest" ]; then
            git clone --depth 1 "https://github.com/$REPO.git" "$src" \
                || err "git clone failed"
        else
            git clone --depth 1 --branch "$VERSION" "https://github.com/$REPO.git" "$src" \
                || err "git clone failed (no such tag: $VERSION?)"
        fi
    fi

    say "building meadowup"
    cargo build --release --manifest-path "$src/installer/Cargo.toml" --bin meadowup \
        || err "cargo build failed"
    replace_binary "$src/installer/target/release/meadowup" meadowup

    # Built from source, so the toolchain is built from the same source rather
    # than downloaded -- a platform with no release build has none to download.
    say "building the toolchain (this takes a minute)"
    cargo install --path "$src/buildtools/meadow" --root "$MEADOW_HOME" --force \
        || err "cargo install failed"
    # meadowup has nothing left to fetch, so stop here rather than hand over.
    VERSION="already-built"
    finish_from_source
}

# What meadowup would have printed, for the from-source path that skips it.
finish_from_source() {
    "$BIN_DIR/meadowup" install --no-modify-path >/dev/null 2>&1 || true
    say ""
    say "installed $("$BIN_DIR/meadow" --version 2>/dev/null || echo meadow)"
    say ""
    say "  meadow                 start the REPL"
    say "  meadow run <path>      build and run a package"
    say "  meadowup update        bring the toolchain up to date"
    say ""
    say "Add to your PATH:  . \"$MEADOW_HOME/env\""
    exit 0
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
