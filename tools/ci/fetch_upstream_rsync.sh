#!/usr/bin/env bash
# fetch_upstream_rsync.sh - the one way CI and the local harnesses obtain an
# upstream rsync release tarball.
#
# Every tarball is checked against its pin in upstream-tarballs.sha256 before
# anything reads it. A download goes to a file, never a pipe: `curl | tar xz`
# turns a truncated transfer (curl exit 18) into a half-extracted tree and a
# gzip error that names neither cause. Verified tarballs are kept in a cache
# directory, which the fetch-upstream-rsync action persists with actions/cache,
# so CI reaches download.samba.org only when a pin changes.
#
# Usage:
#   fetch_upstream_rsync.sh <version> [extract_dir]
#       Ensure rsync-<version>.tar.gz is cached and verified, print its path,
#       and extract it into extract_dir when given.
#   fetch_upstream_rsync.sh --all
#       Ensure every pinned tarball is cached and verified.
#
# Environment:
#   UPSTREAM_TARBALL_CACHE     cache dir (default <repo>/target/interop/upstream-tarballs)
#   UPSTREAM_TARBALL_MANIFEST  pin file (default tools/ci/upstream-tarballs.sha256)
#   RSYNC_TARBALL_BASE_URL     mirror (default https://download.samba.org/pub/rsync/src)
#
# Exit status: 0 ok, 1 download or extraction failed, 2 usage error or version
# not pinned, 3 digest mismatch.

set -euo pipefail

repo_root="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
manifest="${UPSTREAM_TARBALL_MANIFEST:-${repo_root}/tools/ci/upstream-tarballs.sha256}"
cache_dir="${UPSTREAM_TARBALL_CACHE:-${repo_root}/target/interop/upstream-tarballs}"
base_url="${RSYNC_TARBALL_BASE_URL:-https://download.samba.org/pub/rsync/src}"

die() {
    local code=$1
    shift
    printf 'fetch_upstream_rsync: ERROR: %s\n' "$*" >&2
    exit "$code"
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

pinned_sha() {
    local name="rsync-$1.tar.gz" sha
    [[ -f "$manifest" ]] || die 2 "pin manifest not found: ${manifest}"
    sha=$(awk -v n="$name" '$1 !~ /^#/ && $2 == n { print $1 }' "$manifest")
    [[ -n "$sha" ]] || die 2 "rsync $1 has no pinned sha256 in ${manifest}; add one (see its header)"
    printf '%s\n' "$sha"
}

pinned_versions() {
    awk '$1 !~ /^#/ && NF == 2 { v = $2; sub(/^rsync-/, "", v); sub(/\.tar\.gz$/, "", v); print v }' "$manifest"
}

# Leaves a verified rsync-<version>.tar.gz in the cache and prints its path.
# Runs inside $(...), where bash does not inherit errexit, so every step that
# can fail propagates its status explicitly.
ensure_tarball() {
    local version=$1 want got
    want=$(pinned_sha "$version") || exit
    local tarball="${cache_dir}/rsync-${version}.tar.gz"

    if [[ -f "$tarball" ]]; then
        got=$(sha256_of "$tarball") || die 1 "cannot hash ${tarball}"
        if [[ "$got" == "$want" ]]; then
            printf '%s\n' "$tarball"
            return
        fi
        echo "fetch_upstream_rsync: cached ${tarball} has sha256 ${got}, pin is ${want}; discarding it" >&2
        rm -f "$tarball"
    fi

    mkdir -p "$cache_dir" || die 1 "cannot create ${cache_dir}"
    local part="${tarball}.part.$$"
    local url="${base_url}/rsync-${version}.tar.gz"
    echo "fetch_upstream_rsync: downloading ${url}" >&2
    if ! curl -fsSL --connect-timeout 30 --max-time 300 -o "$part" "$url"; then
        rm -f "$part"
        die 1 "download of ${url} failed"
    fi
    got=$(sha256_of "$part") || die 1 "cannot hash ${part}"
    if [[ "$got" != "$want" ]]; then
        rm -f "$part"
        die 3 "sha256 mismatch for ${url}: got ${got}, pinned ${want} in ${manifest}"
    fi
    mv -f "$part" "$tarball" || die 1 "cannot move ${part} into place"
    printf '%s\n' "$tarball"
}

case "${1:-}" in
    "")
        die 2 "usage: fetch_upstream_rsync.sh <version> [extract_dir] | --all"
        ;;
    --all)
        [[ $# -eq 1 ]] || die 2 "--all takes no further arguments"
        [[ -f "$manifest" ]] || die 2 "pin manifest not found: ${manifest}"
        while read -r version; do
            (ensure_tarball "$version" >/dev/null) || exit
        done < <(pinned_versions)
        ;;
    *)
        [[ $# -le 2 ]] || die 2 "usage: fetch_upstream_rsync.sh <version> [extract_dir] | --all"
        tarball=$(ensure_tarball "$1") || exit
        if [[ -n "${2:-}" ]]; then
            mkdir -p "$2"
            tar -xzf "$tarball" -C "$2" || die 1 "extracting ${tarball} into $2 failed"
        fi
        printf '%s\n' "$tarball"
        ;;
esac
