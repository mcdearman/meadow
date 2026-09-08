#!/bin/sh
# Meadow installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/mcdearman/meadow/master/install.sh | sh
#
# Downloads a prebuilt `meadow` for this platform from GitHub Releases and puts
# it in ~/.meadow/bin. If there is no release build for this platform (or you
# pass --from-source) it builds from source instead, which needs a Rust
# toolchain. Run with --help for the options.

set -eu

REPO="mcdearman/meadow"
MEADOW_HOME="${MEADOW_HOME:-$HOME/.meadow}"
BIN_DIR="$MEADOW_HOME/bin"
ENV_FILE="$MEADOW_HOME/env"

VERSION="latest"
FROM_SOURCE=0
MODIFY_PATH=1
UNINSTALL=0

main() {
    parse_args "$@"

    if [ "$UNINSTALL" -eq 1 ]; then
        uninstall
        return
    fi

    say "installing meadow to $BIN_DIR"
    mkdir -p "$BIN_DIR"

    if [ "$FROM_SOURCE" -eq 1 ]; then
        install_from_source
    else
        target="$(detect_target)"
        if ! install_prebuilt "$target"; then
            say "no prebuilt binary for $target — building from source"
            install_from_source
        fi
    fi

    write_env
    [ "$MODIFY_PATH" -eq 1 ] && add_to_path

    installed="$("$BIN_DIR/meadow" --version 2>/dev/null || echo meadow)"
    say ""
    say "installed $installed"
    say ""
    say "  meadow                 start the REPL"
    say "  meadow run <path>      build and run a package"
    say "  meadow build --release build with release checks"
    say ""
    if [ "$MODIFY_PATH" -eq 1 ]; then
        say "Open a new shell, or run:  . \"$ENV_FILE\""
    else
        say "Add to your PATH:  . \"$ENV_FILE\""
    fi
}

# --- argument parsing -------------------------------------------------------

parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --version)
                [ $# -ge 2 ] || err "--version needs a tag, e.g. --version v0.1.0"
                VERSION="$2"
                shift 2
                ;;
            --from-source) FROM_SOURCE=1; shift ;;
            --no-modify-path) MODIFY_PATH=0; shift ;;
            --uninstall) UNINSTALL=1; shift ;;
            -h|--help) usage; exit 0 ;;
            *) err "unknown option: $1 (try --help)" ;;
        esac
    done
}

usage() {
    cat <<EOF
Install meadow.

USAGE:
    install.sh [OPTIONS]

OPTIONS:
        --version <tag>    Install a specific release (default: latest)
        --from-source      Build from source with cargo instead of downloading
        --no-modify-path   Do not touch your shell profile
        --uninstall        Remove meadow and its PATH entry
    -h, --help             Print this help

ENVIRONMENT:
    MEADOW_HOME    Where to install (default: \$HOME/.meadow)
EOF
}

# --- platform detection -----------------------------------------------------

# Print the Rust target triple for this machine, or fail if it is not one we
# publish binaries for.
detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)  os_part="unknown-linux-gnu" ;;
        Darwin) os_part="apple-darwin" ;;
        *) err "unsupported OS: $os (try --from-source)" ;;
    esac

    case "$arch" in
        x86_64|amd64)  arch_part="x86_64" ;;
        arm64|aarch64) arch_part="aarch64" ;;
        *) err "unsupported architecture: $arch (try --from-source)" ;;
    esac

    echo "${arch_part}-${os_part}"
}

# --- installing -------------------------------------------------------------

# Download and unpack the release archive for $1. Returns non-zero (without
# exiting) when there is no such asset, so the caller can fall back to source.
install_prebuilt() {
    target="$1"
    asset="meadow-${target}.tar.gz"
    if [ "$VERSION" = "latest" ]; then
        url="https://github.com/$REPO/releases/latest/download/$asset"
    else
        url="https://github.com/$REPO/releases/download/$VERSION/$asset"
    fi

    say "downloading $url"
    tmp="$(mktemp -d)"
    # shellcheck disable=SC2064
    trap "rm -rf \"$tmp\"" EXIT

    if ! download "$url" "$tmp/$asset"; then
        rm -rf "$tmp"
        trap - EXIT
        return 1
    fi

    tar -xzf "$tmp/$asset" -C "$tmp" || err "could not unpack $asset"
    [ -f "$tmp/meadow" ] || err "$asset did not contain a meadow binary"

    replace_binary "$tmp/meadow"

    rm -rf "$tmp"
    trap - EXIT
}

# Put `$1` at $BIN_DIR/meadow, replacing whatever is there.
#
# The old binary is renamed out of the way first: writing over an executable that
# is currently running fails on Linux (ETXTBSY), while renaming it never does, so
# an upgrade works even with a REPL open elsewhere. Unlinking a busy file is fine
# on Unix, so the leftover goes immediately.
replace_binary() {
    if [ -f "$BIN_DIR/meadow" ]; then
        rm -f "$BIN_DIR/meadow.old"
        mv "$BIN_DIR/meadow" "$BIN_DIR/meadow.old" 2>/dev/null || true
    fi
    install -m 755 "$1" "$BIN_DIR/meadow" 2>/dev/null \
        || { cp "$1" "$BIN_DIR/meadow" && chmod 755 "$BIN_DIR/meadow"; }
    rm -f "$BIN_DIR/meadow.old"
}

install_from_source() {
    need_cmd cargo
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

    say "building (this takes a minute)"
    # The CLI lives in its own workspace; `--root` puts the binary in our bin dir
    # rather than ~/.cargo/bin.
    cargo install --path "$src/meadow" --root "$MEADOW_HOME" --force \
        || err "cargo install failed"
}

# --- PATH -------------------------------------------------------------------

write_env() {
    cat > "$ENV_FILE" <<EOF
#!/bin/sh
# Adds meadow to PATH. Sourced from your shell profile by the installer.
case ":\${PATH}:" in
    *:"$BIN_DIR":*) ;;
    *) export PATH="$BIN_DIR:\$PATH" ;;
esac
EOF
    chmod 644 "$ENV_FILE"
}

# Source the env file from every shell profile the user actually has, once.
add_to_path() {
    line=". \"$ENV_FILE\""
    added=0
    for profile in "$HOME/.profile" "$HOME/.bash_profile" "$HOME/.bashrc" "$HOME/.zshenv"; do
        [ -f "$profile" ] || continue
        if ! grep -Fqs "$ENV_FILE" "$profile"; then
            printf '\n# added by the meadow installer\n%s\n' "$line" >> "$profile"
            say "added meadow to $profile"
            added=1
        fi
    done
    # No profile at all: make one, so a new shell still finds meadow.
    if [ "$added" -eq 0 ] && [ ! -f "$HOME/.profile" ]; then
        printf '\n# added by the meadow installer\n%s\n' "$line" >> "$HOME/.profile"
        say "added meadow to $HOME/.profile"
    fi
}

uninstall() {
    [ -d "$MEADOW_HOME" ] || err "meadow is not installed at $MEADOW_HOME"
    for profile in "$HOME/.profile" "$HOME/.bash_profile" "$HOME/.bashrc" "$HOME/.zshenv"; do
        [ -f "$profile" ] || continue
        grep -Fqs "$ENV_FILE" "$profile" || continue
        tmp="$(mktemp)"
        # Drop our two lines, then trim trailing blanks so the blank line we
        # printed ahead of them does not pile up over install/uninstall cycles.
        grep -Fv "$ENV_FILE" "$profile" \
            | grep -Fv "# added by the meadow installer" \
            | awk 'NF {last = NR} {line[NR] = $0} END {for (i = 1; i <= last; i++) print line[i]}' \
            > "$tmp"
        cat "$tmp" > "$profile"
        rm -f "$tmp"
        say "removed meadow from $profile"
    done
    rm -rf "$MEADOW_HOME"
    say "removed $MEADOW_HOME"
}

# --- helpers ----------------------------------------------------------------

download() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$1" -O "$2"
    else
        err "need curl or wget to download"
    fi
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || err "need '$1' (not found)
  install it, or install a Rust toolchain from https://rustup.rs"
}

say() { echo "meadow: $1"; }
err() { echo "meadow: error: $1" >&2; exit 1; }

main "$@"
