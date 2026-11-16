#!/bin/sh
# nexo installer for Linux and macOS: downloads the ready-built nexo from the latest GitHub release and puts it
# in ~/.local/bin, on PATH. Where no build fits (another system or processor, or NEXO_BUILD=1), it installs what
# is missing (a C compiler, Rust) and builds nexo instead.
#   curl -fsSL https://raw.githubusercontent.com/Locotir/nexo/main/install.sh | sh
# Settings (environment): NEXO_INSTALL_DIR = the folder to install into (default ~/.local/bin);
# NEXO_BUILD=1 = build from source even where a ready-built file exists.
# Ready-built files exist for Linux and macOS, each on x86_64 and aarch64; other systems build from source.
set -eu

repo="https://github.com/Locotir/nexo"
dir="${NEXO_INSTALL_DIR:-$HOME/.local/bin}"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

os="$(uname -s)"

say() { printf 'nexo: %s\n' "$*"; }

# root does not need sudo; anyone else does for the package manager
as_root() {
    if [ "$(id -u)" = 0 ]; then "$@"; else sudo "$@"; fi
}

# the SHA-256 of a file: Linux has sha256sum, macOS only shasum
sha256() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi
}

# the processor, as the release names it. A terminal running under Rosetta on an Apple silicon Mac reports
# x86_64, but the machine runs the arm64 file natively and faster.
cpu() {
    case "$(uname -m)" in
    x86_64 | amd64)
        if [ "$os" = Darwin ] && [ "$(/usr/sbin/sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
            echo aarch64
        else
            echo x86_64
        fi
        ;;
    aarch64 | arm64) echo aarch64 ;;
    *) return 1 ;;
    esac
}

# the release file for this system and processor, checked against the release's SHA-256 list;
# the release has Linux and macOS files only, so any other system must not get one (it would not run)
from_release() {
    arch="$(cpu)" || return 1
    case "$os-$arch" in
    Linux-x86_64) name=nexo-linux-x86_64 ;;
    Linux-aarch64) name=nexo-linux-aarch64 ;;
    Darwin-x86_64) name=nexo-macos-x86_64 ;;
    Darwin-aarch64) name=nexo-macos-aarch64 ;;
    *) return 1 ;;
    esac
    command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 || return 1
    say "downloading $name from the latest release"
    curl -fsSL "$repo/releases/latest/download/SHA256SUMS" -o "$stage/SHA256SUMS" || return 1
    curl -fsSL "$repo/releases/latest/download/$name" -o "$stage/$name" || return 1
    grep " \*\{0,1\}$name\$" "$stage/SHA256SUMS" >"$stage/want" || return 1
    want="$(cut -d' ' -f1 "$stage/want")"
    got="$(sha256 "$stage/$name" | cut -d' ' -f1)"
    if [ -z "$want" ] || [ "$want" != "$got" ]; then
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
    # on macOS /usr/bin/cc exists even without the developer tools, so ask for the tools themselves
    if [ "$os" = Darwin ]; then
        if ! xcode-select -p >/dev/null 2>&1; then
            say "no C compiler found: run xcode-select --install, then run this again."
            exit 1
        fi
    elif ! command -v cc >/dev/null 2>&1; then
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
    if [ -z "${NEXO_BUILD:-}" ]; then
        if [ "$os" != Linux ]; then
            say "there is no ready-built nexo for $os $(uname -m): trying to build it from source here"
        else
            say "no ready-built release fits here: building from source"
        fi
    fi
    from_source
fi

mkdir -p "$dir"
# a new file put in place by rename: a running nexo keeps working until it restarts
cp "$stage/nexo" "$dir/.nexo.new"
chmod 755 "$dir/.nexo.new"
mv -f "$dir/.nexo.new" "$dir/nexo"
# a file saved by a browser carries macOS's quarantine mark; this one came through curl, so it is only a safeguard
if [ "$os" = Darwin ]; then xattr -d com.apple.quarantine "$dir/nexo" 2>/dev/null || true; fi

# older installs left nexo in cargo's own folder, where it would come first or confuse
cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
if [ -z "${NEXO_INSTALL_DIR:-}" ] && [ "$cargo_bin" != "$dir" ] && [ -e "$cargo_bin/nexo" ]; then
    { command -v cargo >/dev/null 2>&1 && cargo uninstall --quiet nexo-cli >/dev/null 2>&1; } || rm -f "$cargo_bin/nexo"
    say "removed the old copy in $cargo_bin"
fi

# put the folder on PATH for new terminals, once
case ":$PATH:" in
*":$dir:"*) ;;
*)
    line="export PATH=\"$dir:\$PATH\"  # nexo"
    changed=0
    zdir="${ZDOTDIR:-$HOME}"
    for rc in "$HOME/.profile" "$HOME/.bash_profile" "$HOME/.bashrc" "$zdir/.zshrc"; do
        case "$rc" in
        "$HOME/.profile") ;;
        "$HOME/.bash_profile")
            # a macOS bash login shell reads this first and then ignores .profile
            [ "$os" = Darwin ] && [ -e "$rc" ] || continue
            ;;
        "$zdir/.zshrc")
            # zsh does not read .profile: a zsh user with no .zshrc gets one
            case "${SHELL:-}" in
            *zsh) mkdir -p "$zdir" ;;
            *) [ -e "$rc" ] || continue ;;
            esac
            ;;
        *) [ -e "$rc" ] || continue ;;
        esac
        grep -qsF "$line" "$rc" || { printf '\n%s\n' "$line" >>"$rc" && changed=1; }
    done
    if [ -d "$HOME/.config/fish" ]; then
        mkdir -p "$HOME/.config/fish/conf.d"
        fish_line="fish_add_path -g $dir"
        grep -qsxF "$fish_line" "$HOME/.config/fish/conf.d/nexo.fish" || {
            printf '%s\n' "$fish_line" >"$HOME/.config/fish/conf.d/nexo.fish" && changed=1
        }
    fi
    PATH="$dir:$PATH"
    if [ "$changed" = 1 ]; then
        say "added $dir to PATH for new terminals"
    else
        say "$dir is already set on PATH for new terminals"
    fi
    ;;
esac

found="$(command -v nexo 2>/dev/null || true)"
if [ "$found" = "$dir/nexo" ]; then
    say "installed: $dir/nexo. Run: nexo  (a terminal opened before this needs reopening)"
else
    say "installed: $dir/nexo, but another nexo comes first on your PATH: $found"
    say "remove that one, or run $dir/nexo"
fi
