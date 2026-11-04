#!/bin/sh
# nexo installer for Linux: builds nexo from source with cargo.
#   curl -fsSL https://raw.githubusercontent.com/Locotir/nexo/main/install.sh | sh
# NEXO_INSTALL_RUST=1 installs Rust (rustup) without asking when it is missing.
set -eu

repo="https://github.com/Locotir/nexo"

say() { printf 'nexo: %s\n' "$*"; }

# rustup puts cargo here, often before the shell's PATH knows it
if ! command -v cargo >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/cargo" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
fi

if ! command -v cargo >/dev/null 2>&1; then
    say "nexo is built with Rust, and cargo was not found."
    answer="${NEXO_INSTALL_RUST:-}"
    # the script itself comes on stdin when piped: ask on the terminal
    if [ -z "$answer" ] && [ -r /dev/tty ]; then
        printf 'nexo: install Rust now with rustup (https://rustup.rs)? [y/N] '
        read -r answer </dev/tty || answer=""
    fi
    case "$answer" in
        y | Y | yes | 1) ;;
        *)
            say "install Rust from https://rustup.rs, then run this again."
            exit 1
            ;;
    esac
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    PATH="$HOME/.cargo/bin:$PATH"
fi

if ! command -v cc >/dev/null 2>&1; then
    say "no C compiler (cc) found; the build needs one: install gcc or clang (build-essential on Debian and Ubuntu)."
    exit 1
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
    say "installed in $bin, which is not on your PATH. Add it:"
    # shellcheck disable=SC2016 # $PATH is for the person to paste
    printf '    export PATH="%s:$PATH"\n' "$bin"
fi
