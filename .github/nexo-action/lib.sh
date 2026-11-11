#!/usr/bin/env bash
# Shared helpers for the nexo action scripts. Sourced, never run alone.

NEXO_REPO="${NEXO_REPO:-Locotir/nexo}"
case "$NEXO_REPO" in */*/* | */ | /* | *[!A-Za-z0-9._/-]* | "") printf "::error::nexo-action: invalid NEXO_REPO\n" >&2; exit 1 ;; esac

log() { printf 'nexo-action: %s\n' "$*" >&2; }
die() { printf '::error::nexo-action: %s\n' "$*" >&2; exit 1; }
# scrub: nexo output goes to the job log; a line that starts with "::" would be read as a workflow command
scrub() { sed -u "s/^[[:space:]]*::/ _::/"; }

# set_output NAME VALUE: writes one action output (multi-line safe)
set_output() {
  local name="$1" value="$2" delim
  [ -n "${GITHUB_OUTPUT:-}" ] || return 0
  # the value may be model text: pick a delimiter that does not occur in it
  while :; do
    delim="NEXO_EOF_$RANDOM$RANDOM$RANDOM$$"
    case "$value" in *"$delim"*) ;; *) break ;; esac
  done
  { printf '%s<<%s\n' "$name" "$delim"; printf '%s\n' "$value"; printf '%s\n' "$delim"; } >>"$GITHUB_OUTPUT"
}

# prepare_env: turns the api_key input into the environment variable nexo reads.
# The key only ever lives in this process environment, never on a command line.
prepare_env() {
  if [ -n "${NEXO_API_KEY:-}" ]; then
    local var="${NEXO_API_KEY_ENV:-}"
    if [ -z "$var" ]; then
      [ -n "${NEXO_PROVIDER:-}" ] || die "api_key is set but provider is empty: set provider (or api_key_env)"
      var="$(printf '%s' "$NEXO_PROVIDER" | tr '[:lower:]-' '[:upper:]_')_API_KEY"
    fi
    case "$var" in *[!A-Z0-9_]* | '') die "invalid api_key_env name" ;; esac
    export "$var=$NEXO_API_KEY"
    printf '::add-mask::%s\n' "$NEXO_API_KEY"
  fi
  unset NEXO_API_KEY
}

# global_args: provider and model flags, one per line
global_args() {
  [ -z "${NEXO_PROVIDER:-}" ] || printf '%s\n%s\n' --provider "$NEXO_PROVIDER"
  [ -z "${NEXO_MODEL:-}" ] || printf '%s\n%s\n' --model "$NEXO_MODEL"
  return 0
}

# write_access LOGIN: succeeds when the user has write, maintain or admin on the repository
write_access() {
  local perm
  case "$1" in '' | *[!A-Za-z0-9-]*) return 1 ;; esac  # a login is letters, digits and dashes: nothing else goes into the API path
  perm="$(gh api "repos/$GITHUB_REPOSITORY/collaborators/$1/permission" --jq .permission 2>/dev/null)" || return 1
  case "$perm" in admin | maintain | write) return 0 ;; *) return 1 ;; esac
}

# cost_line JSON: "cost $0.0031, 4 turns, model x" from a nexo json result
cost_line() {
  printf '%s' "$1" | jq -r '"cost $" + ((.cost_usd // 0) | tostring) + ", " + ((.num_turns // 0) | tostring) + " turns, model " + (.model // "unknown")'
}
