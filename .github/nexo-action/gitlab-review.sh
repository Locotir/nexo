#!/usr/bin/env bash
# GitLab CI: runs `nexo review --json` on a merge request and posts the findings through the GitLab API.
# One summary note (edited in place) plus inline discussions (the old ones are deleted first).
# Auth: NEXO_GITLAB_TOKEN (project access token, scope api) if set, else CI_JOB_TOKEN.
set -euo pipefail

MARK='<!-- nexo-review -->'
: "${CI_API_V4_URL:?not running in GitLab CI}" "${CI_PROJECT_ID:?}" "${CI_MERGE_REQUEST_IID:?run on merge request pipelines}"
base="${CI_MERGE_REQUEST_TARGET_BRANCH_NAME:?}"
case "$base" in *[!A-Za-z0-9._/-]* | '' | -*) echo "unusable base branch" >&2; exit 1 ;; esac

# the token goes through a curl config on stdin, never on the command line
if [ -n "${NEXO_GITLAB_TOKEN:-}" ]; then hdr="PRIVATE-TOKEN: $NEXO_GITLAB_TOKEN"; else hdr="JOB-TOKEN: ${CI_JOB_TOKEN:?}"; fi
gl() { # gl METHOD PATH [curl args]
  local m="$1" p="$2"; shift 2
  printf 'header = "%s"\n' "$hdr" | curl -fsS -K - -X "$m" "$CI_API_V4_URL/projects/$CI_PROJECT_ID/merge_requests/$CI_MERGE_REQUEST_IID/$p" "$@"
}

if [ -n "${NEXO_API_KEY:-}" ]; then
  var="${NEXO_API_KEY_ENV:-}"
  if [ -z "$var" ]; then
    [ -n "${NEXO_PROVIDER:-}" ] || { echo "set NEXO_PROVIDER or NEXO_API_KEY_ENV" >&2; exit 1; }
    var="$(printf '%s' "$NEXO_PROVIDER" | tr '[:lower:]-' '[:upper:]_')_API_KEY"
  fi
  case "$var" in *[!A-Z0-9_]* | '') echo "invalid key variable name" >&2; exit 1 ;; esac
  export "$var=$NEXO_API_KEY"
fi
unset NEXO_API_KEY

git fetch --no-tags -q origin "+refs/heads/$base:refs/remotes/origin/$base"
gargs=()
[ -z "${NEXO_PROVIDER:-}" ] || gargs+=(--provider "$NEXO_PROVIDER")
[ -z "${NEXO_MODEL:-}" ] || gargs+=(--model "$NEXO_MODEL")
out="$(mktemp)"; err="$(mktemp)"
set +e
nexo "${gargs[@]}" review --base "origin/$base" --json >"$out" 2>"$err"
code=$?
set -e
cat "$err" >&2
if [ "$code" -ne 0 ] && [ "$code" -ne 1 ]; then
  grep -q 'nothing to review' "$err" && { echo "nothing to review"; exit 0; }
  echo "nexo review failed with exit code $code" >&2; exit 1
fi
jq -e '.findings | type == "array"' "$out" >/dev/null

sha="${CI_COMMIT_SHA:-unknown}"
summary="$(jq -r --arg mark "$MARK" --arg sha "${sha:0:7}" '
  def sev: (.severity // "low") | ascii_downcase;
  $mark + "\n## nexo review\n\n"
  + (if (.findings | length) == 0 then "No findings in `" + $sha + "`.\n"
     else (.findings | length | tostring) + " finding(s) in `" + $sha + "`.\n\n"
       + ([.findings[] | "- **" + (sev | ascii_upcase) + "**"
           + (if (.file // "") != "" then " `" + .file + (if .line != null then ":" + (.line | tostring) else "" end) + "`" else "" end)
           + " " + (.title // "") + (if (.detail // "") != "" then "\n  " + (.detail | gsub("\n"; "\n  ")) else "" end)
          ] | join("\n")) + "\n" end)' "$out")"

# summary note: edit ours or create it
old="$(gl GET "notes?per_page=100&sort=asc" | jq -r --arg m "$MARK" '[.[] | select(.body | contains($m)) | select(.system | not)] | .[0].id // empty')"
if [ -n "$old" ]; then
  gl PUT "notes/$old" --data-urlencode "body=$summary" >/dev/null
else
  gl POST notes --data-urlencode "body=$summary" >/dev/null
fi

# inline discussions: delete ours from earlier pushes, then post the current ones
gl GET "discussions?per_page=100" | jq -r --arg m "$MARK" '.[] | .id as $d | .notes[] | select((.body // "") | contains($m)) | select(.position != null) | "\($d) \(.id)"' |
  while read -r d n; do
    [ -z "$n" ] || gl DELETE "discussions/$d/notes/$n" >/dev/null 2>&1 || true
  done
refs="$(gl GET "" | jq -c '.diff_refs // empty')"
if [ -n "$refs" ]; then
  jq -c '.findings[] | select((.file // "") != "" and .line != null)' "$out" | while read -r f; do
    text="$(jq -r --arg m "$MARK" '$m + "\n**" + ((.severity // "low") | ascii_upcase) + "** " + (.title // "") + "\n\n" + (.detail // "")' <<<"$f")"
    gl POST discussions \
      --data-urlencode "body=$text" \
      --data-urlencode "position[position_type]=text" \
      --data-urlencode "position[base_sha]=$(jq -r .base_sha <<<"$refs")" \
      --data-urlencode "position[start_sha]=$(jq -r .start_sha <<<"$refs")" \
      --data-urlencode "position[head_sha]=$(jq -r .head_sha <<<"$refs")" \
      --data-urlencode "position[new_path]=$(jq -r .file <<<"$f")" \
      --data-urlencode "position[new_line]=$(jq -r .line <<<"$f")" >/dev/null 2>&1 ||
      echo "inline note refused for $(jq -r .file <<<"$f") (line outside the diff?); it stays in the summary" >&2
  done
fi

if [ "$(jq '.high' "$out")" = true ] && [ "${NEXO_FAIL_ON_HIGH:-false}" = true ]; then
  echo "high finding(s) found and NEXO_FAIL_ON_HIGH is true" >&2; exit 1
fi
