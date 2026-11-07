#!/usr/bin/env bash
# prompt mode: runs `nexo -p` once and exposes result, cost and exit code as outputs.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$here/lib.sh"

[ -n "${NEXO_PROMPT:-}" ] || die "prompt mode needs the prompt input"
prepare_env

args=(-p "$NEXO_PROMPT" --output-format json)
mapfile -t g < <(global_args)
args+=("${g[@]}")
[ -z "${NEXO_MAX_TURNS:-}" ] || args+=(--max-turns "$NEXO_MAX_TURNS")
[ -z "${NEXO_ALLOWED_TOOLS:-}" ] || args+=(--allowed-tools "$NEXO_ALLOWED_TOOLS")

out="$(mktemp)"
set +e
nexo "${args[@]}" >"$out"
code=$?
set -e

if jq -e . "$out" >/dev/null 2>&1; then
  result="$(jq -r '.result // .error // ""' "$out")"
  cost="$(jq -r '.cost_usd // 0' "$out")"
else
  result=""
  cost=0
fi
set_output result "$result"
set_output cost "$cost"
set_output exit_code "$code"
log "exit code $code, cost \$$cost"
{
  echo "### nexo"
  echo
  echo "exit code $code, cost \$$cost"
} >>"${GITHUB_STEP_SUMMARY:-/dev/null}"
exit "$code"
