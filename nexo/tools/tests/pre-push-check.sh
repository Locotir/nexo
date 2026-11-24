#!/bin/sh
# Exercise the checker without compiling, network access, or changing the real toolchain.
set -eu
source_checker=$(cd "$(dirname "$0")/.." && pwd)/pre-push-check.sh
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
# Exercise the real checker in an isolated tree with non-recursive suite stubs.
mkdir -p "$tmp/bin" "$tmp/tree/tools/tests"
checker=$tmp/tree/tools/pre-push-check.sh
cp "$source_checker" "$checker"
cat > "$tmp/tree/tools/tests/pre-push-check.sh" <<'STUB'
printf '%s\n' shell >> "$SUITE_CALLS"
[ "${FAIL_SUITE:-}" != shell ]
STUB
cat > "$tmp/tree/tools/tests/pre-push-process-signals.py" <<'STUB'
import os
with open(os.environ['SUITE_CALLS'], 'a') as calls:
    calls.write('python\n')
raise SystemExit(os.environ.get('FAIL_SUITE') == 'python')
STUB
cat > "$tmp/bin/cargo" <<'MOCK'
#!/bin/sh
printf '%s|COLORTERM=%s\n' "$*" "${COLORTERM:-}" >> "$CALLS"
if [ -n "${CARGO_SIGNAL_AT:-}" ] && [ "$*" = "$CARGO_SIGNAL_AT" ]; then
    kill -s "$CARGO_SIGNAL" "$PPID"
fi
case "${FAIL:-}:$*" in
    test:test\ *|fmt:fmt\ *|advisories:deny\ *check\ advisories|msrv:+1.89.0\ check\ *|missing:deny\ --version) exit 1 ;;
esac
MOCK
cat > "$tmp/bin/uname" <<'MOCK'
#!/bin/sh
printf '%s\n' "${HOST:-Linux}"
MOCK
cat > "$tmp/bin/gcc" <<'MOCK'
#!/bin/sh
cat >/dev/null
if [ -n "${GCC_SIGNAL:-}" ]; then
    for arg do probe=$arg; done
    printf '%s\n' "$probe" > "$PROBE_LOG"
    kill -s "$GCC_SIGNAL" "$PPID"
fi
exit "${GCC_FAIL:-0}"
MOCK
chmod +x "$tmp/bin/"*
export PATH="$tmp/bin:$PATH" CALLS="$tmp/calls" SUITE_CALLS="$tmp/suite-calls"
unset NEXO_UPDATE_GOLDEN NEXO_BLESS FAIL GCC_FAIL GCC_SIGNAL HOST CARGO_SIGNAL_AT CARGO_SIGNAL FAIL_SUITE

check() {
    label=$1 expected=$2
    shift 2
    : > "$CALLS"
    : > "$SUITE_CALLS"
    status=0
    "$@" > "$tmp/output" 2>&1 || status=$?
    if [ "$status" -ne "$expected" ]; then
        cat "$tmp/output" >&2
        echo "FAIL $label: expected $expected, got $status" >&2
        exit 1
    fi
    printf 'ok: %s\n' "$label"
}
contains() { grep -F -- "$1" "$CALLS" >/dev/null; }
check default 0 sh "$checker"
[ "$(cat "$SUITE_CALLS")" = "$(printf 'shell\npython')" ]
[ "$(wc -l < "$CALLS")" -eq 4 ]
contains 'build --workspace --locked|COLORTERM=truecolor'
contains 'test --workspace --locked --no-fail-fast|COLORTERM=truecolor'
contains 'clippy --workspace --locked --all-targets --keep-going -- -D warnings'
check full 0 sh "$checker" --full
[ "$(wc -l < "$CALLS")" -eq 9 ]
contains '+1.89.0 check --workspace --locked --all-targets'
contains 'deny --all-features --locked check bans licenses sources'
contains 'deny --all-features --locked check advisories'
for suite in shell python; do
    for mode in default full; do
        if [ "$mode" = full ]; then
            check "$suite-regression-failure-$mode" 1 env FAIL_SUITE="$suite" sh "$checker" --full
        else
            check "$suite-regression-failure-$mode" 1 env FAIL_SUITE="$suite" sh "$checker"
        fi
        [ "$(cat "$SUITE_CALLS")" = "$(printf 'shell\npython')" ]
        contains 'clippy --workspace'
        ! grep -F 'Requested local checks passed' "$tmp/output" >/dev/null
    done
done
check test-failure-still-lints 1 env FAIL=test sh "$checker" --full
contains 'clippy --workspace'
contains 'deny --all-features --locked check advisories'
check fmt-failure-still-tests 1 env FAIL=fmt sh "$checker"
contains 'test --workspace'
check advisories-fail-locally 1 env FAIL=advisories sh "$checker" --full
check msrv-failure-still-checks-policy 1 env FAIL=msrv sh "$checker" --full
contains 'deny --all-features --locked check advisories'
check missing-deny-stops-early 1 env FAIL=missing sh "$checker" --full
[ "$(wc -l < "$CALLS")" -eq 2 ]
check missing-multilib-stops-early 1 env GCC_FAIL=1 sh "$checker"
[ ! -s "$CALLS" ]
# A cancelled prerequisite must clean up and never continue to build or validate.
for signal in HUP INT TERM; do
    case "$signal" in HUP) expected=129 ;; INT) expected=130 ;; TERM) expected=143 ;; esac
    check "cancelled-probe-$signal" "$expected" env GCC_SIGNAL="$signal" PROBE_LOG="$tmp/probe" sh "$checker"
    [ ! -s "$CALLS" ]
    [ ! -e "$(dirname "$(cat "$tmp/probe")")" ]
done
# Cancellation must stop on every host and phase, not just the Linux gcc probe.
for host in Linux Darwin; do
    for signal in HUP INT TERM; do
        case "$signal" in HUP) expected=129 ;; INT) expected=130 ;; TERM) expected=143 ;; esac
        for phase in preflight build test clippy; do
            case "$phase" in
                preflight) command='+1.89.0 --version'; calls=1 ;;
                build) command='build --workspace --locked'; calls=4 ;;
                test) command='test --workspace --locked --no-fail-fast'; calls=5 ;;
                clippy) command='clippy --workspace --locked --all-targets --keep-going -- -D warnings'; calls=6 ;;
            esac
            check "cancelled-$host-$phase-$signal" "$expected" env HOST="$host" \
                CARGO_SIGNAL_AT="$command" CARGO_SIGNAL="$signal" sh "$checker" --full
            [ "$(wc -l < "$CALLS")" -eq "$calls" ]
            if grep -F 'Requested local checks passed' "$tmp/output" >/dev/null; then
                echo 'FAIL: cancellation announced successful validation' >&2
                exit 1
            fi
        done
    done
done
check non-linux-host 0 env HOST=Darwin GCC_FAIL=1 sh "$checker"
# Several TUI snapshot tests use var(...).is_ok(): even "0" or empty enables writes.
for value in 1 0 '' false; do
    check "golden-refused-value-$value" 1 env NEXO_UPDATE_GOLDEN="$value" sh "$checker"
    [ ! -s "$CALLS" ]
    grep -F 'tests must not rewrite expected output' "$tmp/output" >/dev/null
done
# The subagent-events fixture uses var_os, so even an empty value blesses it.
for value in 1 0 '' false; do
    for mode in default full; do
        if [ "$mode" = full ]; then
            check "bless-refused-$mode-$value" 1 env NEXO_BLESS="$value" sh "$checker" --full
        else
            check "bless-refused-$mode-$value" 1 env NEXO_BLESS="$value" sh "$checker"
        fi
        [ ! -s "$CALLS" ]
        grep -F 'tests must not rewrite expected output' "$tmp/output" >/dev/null
    done
done
check unknown-flag 2 sh "$checker" --unknown
[ ! -s "$CALLS" ]
check extra-argument 2 sh "$checker" --full extra
[ ! -s "$CALLS" ]
printf '%s\n' 'All pre-push checker tests passed.'
