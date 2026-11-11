#!/usr/bin/env bash
# Resolves and installs the nexo release binary for this runner.
#   install.sh resolve   prints the release tag for $NEXO_VERSION (latest by default)
#   install.sh fetch     downloads that tag into $NEXO_BIN_DIR, checked against SHA256SUMS
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$here/lib.sh"

version="${NEXO_VERSION:-latest}"
[ -n "$version" ] || version=latest

asset_name() {
  local os arch
  case "${RUNNER_OS:-$(uname -s)}" in
    Linux) os=linux ;;
    Windows) os=windows ;;
    *) die "no ready-built nexo for ${RUNNER_OS:-$(uname -s)}: use a Linux or Windows runner" ;;
  esac
  case "${RUNNER_ARCH:-$(uname -m)}" in
    X64 | x86_64 | amd64) arch=x86_64 ;;
    ARM64 | aarch64 | arm64) arch=aarch64 ;;
    *) die "no ready-built nexo for ${RUNNER_ARCH:-$(uname -m)}" ;;
  esac
  if [ "$os" = windows ]; then echo "nexo-$os-$arch.exe"; else echo "nexo-$os-$arch"; fi
}

# https only, bounded time, a few retries; every download goes through this
fetch_url() { curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 --retry-delay 2 --connect-timeout 15 --max-time 300 "$@"; }

# a release tag is data that ends up in a URL and in $GITHUB_OUTPUT: one safe word, nothing else
valid_tag() { case "$1" in build-* ) ;; *) return 1 ;; esac; case "${1#build-}" in '' | *[!A-Za-z0-9._-]*) return 1 ;; esac; }

resolve() {
  local tag
  case "$version" in
    latest)
      # the latest release redirects to /releases/tag/<tag>
      local url
      url="$(fetch_url -o /dev/null -w '%{url_effective}' "https://github.com/$NEXO_REPO/releases/latest")" ||
        die "cannot reach the nexo releases"
      tag="${url##*/}"
      ;;
    *) tag="$version" ;;
  esac
  valid_tag "$tag" || die "nexo_version must be 'latest' or a release tag such as build-1a2b3c4"
  printf '%s\n' "$tag"
}

fetch() {
  local tag name dir
  tag="$(resolve)"
  name="$(asset_name)"
  dir="${NEXO_BIN_DIR:?NEXO_BIN_DIR is not set}"
  mkdir -p "$dir"
  local exe="$dir/nexo"
  [ "${name%.exe}" = "$name" ] || exe="$dir/nexo.exe"
  if [ -x "$exe" ] && [ -f "$dir/.tag" ] && [ "$(cat "$dir/.tag")" = "$tag" ]; then
    log "nexo $tag already in place (cache)"
    return 0
  fi
  stage="$(mktemp -d)"
  # the path is expanded now on purpose; the variable is gone when the trap fires
  # shellcheck disable=SC2064
  trap "rm -rf \"$stage\"" EXIT
  local base="https://github.com/$NEXO_REPO/releases/download/$tag"
  log "downloading $name from $tag"
  fetch_url "$base/SHA256SUMS" -o "$stage/SHA256SUMS" || die "no SHA256SUMS in release $tag"
  fetch_url "$base/$name" -o "$stage/$name" || die "no $name in release $tag"
  grep " \*\{0,1\}$name\$" "$stage/SHA256SUMS" >"$stage/want" || die "$name is not in SHA256SUMS"
  (cd "$stage" && sha256sum -c --status want) || die "$name does not match its SHA-256"
  chmod 755 "$stage/$name"
  mv -f "$stage/$name" "$exe"
  printf '%s\n' "$tag" >"$dir/.tag"
  "$exe" --version >&2
}

case "${1:-}" in
  resolve) resolve ;;
  fetch) fetch ;;
  *) die "usage: install.sh resolve|fetch" ;;
esac
