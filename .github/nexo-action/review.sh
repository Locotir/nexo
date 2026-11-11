#!/usr/bin/env bash
# review mode: runs `nexo review --json` on a pull request and posts the findings.
# One summary comment (edited in place on every push) and one set of inline comments (the old
# set is deleted first), both tagged with a hidden marker so only our own are touched.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$here/lib.sh"

MARK='<!-- nexo-review -->'
event="${GITHUB_EVENT_PATH:?no event file}"
repo="${GITHUB_REPOSITORY:?no repository}"

pr="$(jq -r '.pull_request.number // empty' "$event")"
[ -n "$pr" ] || die "review mode runs on pull_request events only"
base="${NEXO_BASE:-$(jq -r '.pull_request.base.ref' "$event")}"
head_sha="$(jq -r '.pull_request.head.sha' "$event")"
case "$base" in *[!A-Za-z0-9._/-]* | '' | -*) die "unusable base branch name" ;; esac

prepare_env
git fetch --no-tags -q origin "+refs/heads/$base:refs/remotes/origin/$base" ||
  die "cannot fetch origin/$base (use actions/checkout with fetch-depth: 0)"

mapfile -t gargs < <(global_args)
out="$(mktemp)"
err="$(mktemp)"
trap 'rm -f "$out" "$err"' EXIT
set +e
# the model run does not need the GitHub token: keep it out of its environment
env -u GH_TOKEN nexo "${gargs[@]}" review --base "origin/$base" --json >"$out" 2>"$err"
code=$?
set -e
scrub <"$err" >&2
# exit 1 only means "a high finding exists"; the JSON is still valid
if [ "$code" -ne 0 ] && [ "$code" -ne 1 ]; then
  if grep -q 'nothing to review' "$err"; then
    log "nothing to review"
    set_output findings 0
    set_output high false
    exit 0
  fi
  die "nexo review failed with exit code $code"
fi
jq -e '.findings | type == "array"' "$out" >/dev/null || die "nexo review printed no findings list"

total="$(jq '.findings | length' "$out")"
high="$(jq '.high' "$out")"
set_output findings "$total"
set_output high "$high"

# the summary text
summary="$(jq -r --arg mark "$MARK" --arg sha "${head_sha:0:7}" '
  def sev: (.severity // "low") | ascii_downcase;
  def n(s): [.findings[] | select(sev == s)] | length;
  def icon: if sev == "high" or sev == "critical" then "HIGH" elif sev == "medium" then "MEDIUM" else "LOW" end;
  $mark + "\n## nexo review\n\n"
  + (if (.findings | length) == 0 then "No findings in `" + $sha + "`.\n"
     else (.findings | length | tostring) + " finding(s) in `" + $sha + "`: "
       + (n("high") + n("critical") | tostring) + " high, " + (n("medium") | tostring) + " medium, "
       + (n("low") | tostring) + " low.\n\n"
       + ([.findings[] | "- **" + icon + "**"
           + (if (.file // "") != "" then " `" + .file + (if .line != null then ":" + (.line | tostring) else "" end) + "`" else "" end)
           + " " + (.title // "") + (if (.detail // "") != "" then "\n  " + (.detail | gsub("\n"; "\n  ")) else "" end)
          ] | join("\n"))
       + "\n" end)' "$out")"

# inline comments for findings that point at a file and a line
inline="$(jq -c --arg mark "$MARK" '[.findings[] | select((.file // "") != "" and .line != null)
  | {path: .file, line: .line, side: "RIGHT",
     body: ($mark + "\n**" + ((.severity // "low") | ascii_upcase) + "** " + (.title // "") + "\n\n" + (.detail // ""))}]' "$out")"

api() { gh api "$@"; }

# 1) summary: edit our earlier comment or create it
old_id="$(api --paginate "repos/$repo/issues/$pr/comments" --jq ".[] | select(.user.type == \"Bot\" and (.body | contains(\"$MARK\"))) | .id" 2>/dev/null | head -n1 || true)"
body_json="$(jq -n --arg b "$summary" '{body: $b}')"
if [ -n "$old_id" ]; then
  printf '%s' "$body_json" | api -X PATCH "repos/$repo/issues/comments/$old_id" --input - >/dev/null ||
    log "could not update the summary comment (read-only token on a fork?)"
else
  printf '%s' "$body_json" | api -X POST "repos/$repo/issues/$pr/comments" --input - >/dev/null ||
    log "could not post the summary comment (read-only token on a fork?)"
fi

# 2) inline comments: remove our old ones, then post a fresh set in one review
api --paginate "repos/$repo/pulls/$pr/comments" --jq ".[] | select(.user.type == \"Bot\" and (.body | contains(\"$MARK\"))) | .id" 2>/dev/null |
  while read -r cid; do
    [ -z "$cid" ] || api -X DELETE "repos/$repo/pulls/comments/$cid" >/dev/null 2>&1 || true
  done
if [ "$(jq 'length' <<<"$inline")" -gt 0 ]; then
  payload="$(jq -n --arg sha "$head_sha" --argjson c "$inline" '{commit_id: $sha, event: "COMMENT", comments: $c}')"
  if ! printf '%s' "$payload" | api -X POST "repos/$repo/pulls/$pr/reviews" --input - >/dev/null 2>&1; then
    # a line outside the diff makes GitHub refuse the whole review: the summary already lists every finding
    log "inline comments refused (a line outside the diff?); findings stay in the summary comment"
  fi
fi

log "$total finding(s), high=$high"
if [ "$high" = true ] && [ "${NEXO_FAIL_ON_HIGH:-false}" = true ]; then
  die "high finding(s) found and fail_on_high is true"
fi
