#!/bin/sh
# nexo installer for Linux: downloads the ready-built nexo from the latest GitHub release and puts it
# in ~/.local/bin, on PATH. Where no build fits (another processor, or NEXO_BUILD=1), it installs what
# is missing (a C compiler, Rust) and builds nexo instead.
#   curl -fsSL https://raw.githubusercontent.com/Locotir/nexo/main/install.sh | sh
set -eu

repo="https://github.com/Locotir/nexo"
dir="${NEXO_INSTALL_DIR:-$HOME/.local/bin}"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

say() { printf 'nexo: %s\n' "$*"; }

# root does not need sudo; anyone else does for the package manager
as_root() {
    if [ "$(id -u)" = 0 ]; then "$@"; else sudo "$@"; fi
}

# the release file for this processor, checked against the release's SHA-256 list
from_release() {
    case "$(uname -m)" in
    x86_64 | amd64) name=nexo-linux-x86_64 ;;
    aarch64 | arm64) name=nexo-linux-aarch64 ;;
    *) return 1 ;;
    esac
    command -v sha256sum >/dev/null 2>&1 || return 1
    say "downloading $name from the latest release"
    curl -fsSL "$repo/releases/latest/download/SHA256SUMS" -o "$stage/SHA256SUMS" || return 1
    curl -fsSL "$repo/releases/latest/download/$name" -o "$stage/$name" || return 1
    grep " \*\{0,1\}$name\$" "$stage/SHA256SUMS" >"$stage/want" || return 1
    if ! (cd "$stage" && sha256sum -c --status want); then
        say "$name does not match its SHA-256: not used"
        return 1
    fi
    mv "$stage/$name" "$stage/nexo"
}

from_source() {
    # rustup puts cargo here, often before the shell's PATH knows it
    if ! command -v cargo >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/cargo" ]; then
        PATH="$HOME/.cargo/bin:$PATH"
    fi
    if ! command -v cc >/dev/null 2>&1; then
        say "installing a C compiler (the build needs one)"
        if command -v apt-get >/dev/null 2>&1; then
            as_root apt-get update -qq && as_root apt-get install -y -qq build-essential
        elif command -v dnf >/dev/null 2>&1; then
            as_root dnf install -y -q gcc
        elif command -v pacman >/dev/null 2>&1; then
            as_root pacman -S --needed --noconfirm gcc
        elif command -v zypper >/dev/null 2>&1; then
            as_root zypper --non-interactive install gcc
        elif command -v apk >/dev/null 2>&1; then
            as_root apk add --no-cache build-base
        else
            say "no package manager known here: install gcc or clang, then run this again."
            exit 1
        fi
    fi
    if ! command -v cargo >/dev/null 2>&1; then
        say "installing Rust (rustup, https://rustup.rs)"
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
        PATH="$HOME/.cargo/bin:$PATH"
    fi
    say "building the latest nexo (a few minutes the first time)"
    cargo install --quiet --locked --git "$repo" nexo-cli --root "$stage/build"
    mv "$stage/build/bin/nexo" "$stage/nexo"
}

if [ -n "${NEXO_BUILD:-}" ] || ! from_release; then
    [ -n "${NEXO_BUILD:-}" ] || say "no ready-built release fits here: building from source"
    from_source
fi

mkdir -p "$dir"
# a new file put in place by rename: a running nexo keeps working until it restarts
cp "$stage/nexo" "$dir/.nexo.new"
chmod 755 "$dir/.nexo.new"
mv -f "$dir/.nexo.new" "$dir/nexo"

# older installs left nexo in cargo's own folder, where it would come first or confuse
cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
if [ "$cargo_bin" != "$dir" ] && [ -e "$cargo_bin/nexo" ]; then
    { command -v cargo >/dev/null 2>&1 && cargo uninstall --quiet nexo-cli >/dev/null 2>&1; } || rm -f "$cargo_bin/nexo"
    say "removed the old copy in $cargo_bin"
fi

# put the folder on PATH for new terminals, once
case ":$PATH:" in
*":$dir:"*) ;;
*)
    line="export PATH=\"$dir:\$PATH\"  # nexo"
    for rc in "$HOME/.profile" "$HOME/.bashrc" "$HOME/.zshrc"; do
        case "$rc" in
        "$HOME/.profile") ;;
        *) [ -e "$rc" ] || continue ;;
        esac
        grep -qsF "$line" "$rc" || printf '\n%s\n' "$line" >>"$rc"
    done
    if [ -d "$HOME/.config/fish" ]; then
        mkdir -p "$HOME/.config/fish/conf.d"
        printf 'fish_add_path -g %s\n' "$dir" >"$HOME/.config/fish/conf.d/nexo.fish"
    fi
    PATH="$dir:$PATH"
    say "added $dir to PATH for new terminals"
    ;;
esac

found="$(command -v nexo 2>/dev/null || true)"
if [ "$found" = "$dir/nexo" ]; then
    say "installed: $dir/nexo. Run: nexo  (a terminal opened before this needs reopening)"
else
    say "installed: $dir/nexo, but another nexo comes first on your PATH: $found"
    say "remove that one, or run $dir/nexo"
fi
