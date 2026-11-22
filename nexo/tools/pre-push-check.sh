#!/bin/sh
# Local stable-host counterpart of the workspace CI job; see CONTRIBUTING.md for its limits.
set -eu
cd "$(dirname "$0")/.."

if [ "${NEXO_UPDATE_GOLDEN:-0}" != 0 ]; then
    echo 'Refusing to validate with NEXO_UPDATE_GOLDEN set: tests must not rewrite expected output.' >&2
    exit 1
fi

printf '%s\n' 'Local checks only: other operating systems, MSRV, cargo-deny and the Intel Mac target are NOT checked.'
printf '%s\n' 'Inspect every GitHub Actions job separately; macOS and advisories are currently non-gating.'
if [ "$(uname -s)" = Linux ]; then
    # These tests otherwise return early, which can produce a misleading pass.
    command -v gcc >/dev/null 2>&1 || { echo 'Missing gcc: install gcc and 32-bit development libraries.' >&2; exit 1; }
    probe=$(mktemp -d)
    trap 'rm -rf "$probe"' EXIT HUP INT TERM
    if ! printf 'int main(void) { return 0; }\n' | gcc -m32 -x c - -o "$probe/probe" >/dev/null 2>&1; then
        echo 'Cannot compile 32-bit C: install gcc multilib headers and libraries.' >&2
        exit 1
    fi
fi
export COLORTERM=truecolor
# Allow callers to set CARGO_BUILD_JOBS and CARGO_TARGET_DIR for their machine.
cargo fmt --check
cargo build --workspace
cargo test --workspace --no-fail-fast
cargo clippy --workspace --all-targets --keep-going -- -D warnings
printf '%s\n' 'Local stable-host checks passed; this is not a multi-platform CI result.'
