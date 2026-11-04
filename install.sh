#!/bin/sh
# nexo installer for Linux: installs what is missing (Rust, a C compiler), then builds nexo with cargo.
#   curl -fsSL https://raw.githubusercontent.com/Locotir/nexo/main/install.sh | sh
set -eu

repo="https://github.com/Locotir/nexo"

say() { printf 'nexo: %s\n' "$*"; }

# root does not need sudo; anyone else does for the package manager
as_root() {
    if [ "$(id -u)" = 0 ]; then "$@"; else sudo "$@"; fi
}

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
cargo install --locked --git "$repo" nexo-cli

bin="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}/bin"
found="$(command -v nexo 2>/dev/null || true)"
if [ "$found" = "$bin/nexo" ]; then
    say "installed: $found. Run: nexo"
elif [ -n "$found" ]; then
    say "installed in $bin, but another nexo comes first on your PATH: $found"
    say "remove that one, or put $bin first on your PATH."
else
    say "installed in $bin. Open a new terminal (or run the line below), then: nexo"
    # shellcheck disable=SC2016 # $PATH is for the person to paste
    printf '    export PATH="%s:$PATH"\n' "$bin"
fi
