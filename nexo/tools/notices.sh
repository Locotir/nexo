#!/usr/bin/env bash
# Writes THIRD-PARTY-NOTICES.txt: every external package of the cargo workspace (name, version, SPDX licence,
# repository, and the text of its LICENSE / LICENCE / COPYING / NOTICE files). Needs bash, cargo and jq.
#   nexo/tools/notices.sh [output-file]     (default: THIRD-PARTY-NOTICES.txt in the current folder)
# The output depends only on Cargo.lock and the package sources: same input, same bytes.
set -euo pipefail
export LC_ALL=C

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"
case "${1:-THIRD-PARTY-NOTICES.txt}" in
/*) out="${1:-THIRD-PARTY-NOTICES.txt}" ;;
*) out="$PWD/${1:-THIRD-PARTY-NOTICES.txt}" ;;
esac

cd "$root"
cargo fetch --locked >&2
meta="$(cargo metadata --format-version 1 --locked)"

# external packages only (the workspace's own crates are left out), sorted by name then version;
# fields are joined with the unit separator so that an empty field stays a field
rows="$(printf '%s' "$meta" | jq -r '
  (.workspace_members) as $own
  | [.packages[] | select(.id as $i | $own | index($i) | not)]
  | sort_by([.name, .version])
  | .[]
  | [.name, .version, (.license // "UNKNOWN"), (.repository // ""), .manifest_path]
  | join("\u001f")')"

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

{
  cat <<'HEAD'
THIRD-PARTY NOTICES FOR NEXO

nexo is licensed under the MIT License (see the LICENSE file of the release). It is built from the Rust
packages listed below. For each one this file gives its name, version, SPDX licence expression, repository
and the text of the licence and notice files found in the package source. Where a package offers a choice
of licences (OR), you may use it under any one of them.

Packages under MPL-2.0 are used unmodified. Their source code is available from the repository and the
crates.io address given in their entry.

HEAD
  count=0
  while IFS=$'\x1f' read -r name version license repo manifest; do
    [ -n "$name" ] || continue
    dir="$(dirname "$manifest")"
    [ -d "$dir" ] || { echo "notices: no source folder for $name $version: $dir" >&2; exit 1; }
    count=$((count + 1))
    printf '================================================================================\n'
    printf 'Package: %s %s\n' "$name" "$version"
    printf 'License: %s\n' "$license"
    printf 'Repository: %s\n' "${repo:-none listed}"
    case "$license" in
    *MPL-2.0*)
      printf 'Source (MPL-2.0): %s and https://crates.io/crates/%s/%s\n' "${repo:-no repository listed}" "$name" "$version"
      ;;
    esac
    printf '================================================================================\n\n'
    found=0
    while IFS= read -r f; do
      [ -n "$f" ] || continue
      found=1
      printf -- '--- %s ---\n' "$(basename "$f")"
      cat "$f"
      # a file without a final newline still ends its line
      [ -z "$(tail -c1 "$f")" ] || printf '\n'
      printf '\n'
    done < <(find "$dir" -maxdepth 1 \( -type f -o -type l \) \
      \( -iname 'LICENSE*' -o -iname 'LICENCE*' -o -iname 'COPYING*' -o -iname 'NOTICE*' \) | sort)
    if [ "$found" = 0 ]; then
      printf 'No licence file in the package; see the repository.\n\n'
    fi
  done <<<"$rows"
  echo "notices: $count packages" >&2
} >"$tmp"

chmod 644 "$tmp"
mv "$tmp" "$out"
trap - EXIT
