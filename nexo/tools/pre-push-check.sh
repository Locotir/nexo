#!/bin/sh
# Local stable-host counterpart of the workspace CI job; see CONTRIBUTING.md for its limits.
set -eu
cd "$(dirname "$0")/.."

# Install cancellation before any prerequisites, on every host. A cleanup-only
# trap resumes execution; cancellation must not launch more checks or report success.
probe=''
trap '[ -z "$probe" ] || rm -rf "$probe"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

full=0
case "${1:-}" in
    '') ;;
    --full) full=1; shift ;;
    *) echo "Usage: $0 [--full]" >&2; exit 2 ;;
esac
[ "$#" -eq 0 ] || { echo "Usage: $0 [--full]" >&2; exit 2; }
# Snapshot suites treat any present value (including empty or "0") as write mode.
if [ "${NEXO_UPDATE_GOLDEN+x}" = x ] || [ "${NEXO_BLESS+x}" = x ]; then
    echo 'Refusing to validate with NEXO_UPDATE_GOLDEN or NEXO_BLESS set: tests must not rewrite expected output.' >&2
    exit 1
fi

printf '%s\n' 'Local checks only: other operating systems and the Intel Mac target are NOT checked.'
if [ "$full" -eq 0 ]; then
    printf '%s\n' 'MSRV and cargo-deny are NOT checked; use --full to include them.'
else
    # Fail early rather than compile the workspace before discovering a missing tool.
    cargo +1.89.0 --version >/dev/null
    cargo deny --version >/dev/null
fi
printf '%s\n' 'Inspect every GitHub Actions job separately; advisories are currently non-gating in CI.'
if [ "$(uname -s)" = Linux ]; then
    # These tests otherwise return early, which can produce a misleading pass.
    command -v gcc >/dev/null 2>&1 || { echo 'Missing gcc: install gcc and 32-bit development libraries.' >&2; exit 1; }
    probe=$(mktemp -d)
    if ! printf 'int main(void) { return 0; }\n' | gcc -m32 -x c - -o "$probe/probe" >/dev/null 2>&1; then
        echo 'Cannot compile 32-bit C: install gcc multilib headers and libraries.' >&2
        exit 1
    fi
fi
export COLORTERM=truecolor
# Do not stop at the first failure: a failed test must not hide clippy/MSRV/policy diagnostics.
# Run Cargo sequentially: parallel Cargo processes contend for the same target-directory lock.
failed=0
run() {
    printf '\n+ %s\n' "$*"
    if "$@"; then :; else
        printf 'FAILED: %s\n' "$*" >&2
        failed=1
    fi
}
# The regression suites exercise isolated checker copies with stub suites, so they
# cannot recursively invoke themselves. Missing tools or suites must fail validation.
run sh tools/tests/pre-push-check.sh
run python3 tools/tests/pre-push-process-signals.py
# Allow callers to set CARGO_BUILD_JOBS and CARGO_TARGET_DIR for their machine.
run cargo fmt --check
run cargo build --workspace --locked
run cargo test --workspace --locked --no-fail-fast
run cargo clippy --workspace --locked --all-targets --keep-going -- -D warnings
if [ "$full" -eq 1 ]; then
    run cargo +1.89.0 check --workspace --locked --all-targets
    run cargo deny --all-features --locked check bans licenses sources
    # Stricter than CI: do not mistake a fresh advisory for a clean local security check.
    run cargo deny --all-features --locked check advisories
fi
[ "$failed" -eq 0 ] || { echo 'Local checks failed; inspect the diagnostics above.' >&2; exit 1; }
printf '%s\n' 'Requested local checks passed; this is not a multi-platform CI result.'
