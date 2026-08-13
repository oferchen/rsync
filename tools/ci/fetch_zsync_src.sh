#!/usr/bin/env bash
# fetch_zsync_src.sh - fetch the OFFICIAL zsync sources cited by the
# zsync-inspired matching design notes, verifying each published digest.
#
# The four techniques in crates/matching are derived from zsync's librcksum.
# Every citation in docs/design/zsync-*.md and docs/audits/zsync-*.md must be
# checkable against the author's own release, not a third-party GitHub mirror.
# Official project: https://zsync.moria.org.uk (Colin Phipps).
#
# Two releases are pinned, for different reasons:
#
#   0.6.2 (C)  - the release every existing citation targets. librcksum/ is C
#                here: hash.c, rsum.c, state.c, internal.h.
#   0.7.2 (Go) - the current release. 0.7.0 rewrote the client in Go, and
#                rcksum came with it: internal/rcksum/*.go. There is no C
#                librcksum in 0.7.x, so 0.7.2 is a reimplementation, NOT the
#                original the techniques were derived from. It is pinned as the
#                current-behaviour reference, not as the citation target.
#
# Digest algorithms differ because the project publishes different ones per
# release: 0.6.2 carries a sha1sum, 0.7.2 a sha256sum. Both values below are
# transcribed from https://zsync.moria.org.uk/downloads.
#
# Sources land under target/interop/zsync-src/, mirroring the
# target/interop/upstream-src/ convention used for the upstream rsync C source.
#
# Exit codes:
#   0 - both releases present and digest-verified
#   1 - download or digest verification failed
#   2 - a required tool is missing

set -uo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
src_root="${workspace_root}/target/interop/zsync-src"
base_url="${ZSYNC_BASE_URL:-https://zsync.moria.org.uk/download}"

for tool in curl tar; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    printf '%s is required to fetch zsync sources\n' "$tool" >&2
    exit 2
  fi
done

# Prefer coreutils, fall back to the macOS shasum spelling.
digest() {
  local algo=$1 file=$2
  if command -v "sha${algo}sum" >/dev/null 2>&1; then
    "sha${algo}sum" "$file" | awk '{print $1}'
  else
    shasum -a "$algo" "$file" | awk '{print $1}'
  fi
}

# tarball : digest-algorithm : published-digest
releases=(
  "zsync-0.6.2.tar.bz2:1:5e69f084c8adaad6a677b68f7388ae0f9507617a"
  "zsync-0.7.2.tar.gz:256:51a54a2bcf60311f108924b5f8795fb7a8eeeedd0b52f4f634842ea3470978a2"
)

mkdir -p "$src_root" || exit 1

for entry in "${releases[@]}"; do
  tarball=${entry%%:*}
  rest=${entry#*:}
  algo=${rest%%:*}
  expected=${rest##*:}
  dir="${src_root}/${tarball%%.tar.*}"

  if [ -d "$dir" ]; then
    printf '[fetch_zsync_src] %s already present\n' "$(basename "$dir")"
    continue
  fi

  archive="${src_root}/${tarball}"
  if ! curl -fsSL --connect-timeout 30 --max-time 300 "${base_url}/${tarball}" -o "$archive"; then
    printf 'failed to download %s\n' "$tarball" >&2
    exit 1
  fi

  actual=$(digest "$algo" "$archive")
  if [ "$actual" != "$expected" ]; then
    printf 'digest mismatch for %s\n  expected sha%s %s\n  actual   sha%s %s\n' \
      "$tarball" "$algo" "$expected" "$algo" "$actual" >&2
    rm -f "$archive"
    exit 1
  fi
  printf '[fetch_zsync_src] %s sha%s OK\n' "$tarball" "$algo"

  if ! tar xf "$archive" -C "$src_root"; then
    printf 'failed to extract %s\n' "$tarball" >&2
    exit 1
  fi
  rm -f "$archive"
done

printf '[fetch_zsync_src] sources under %s\n' "$src_root"
