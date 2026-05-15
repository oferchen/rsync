#!/usr/bin/env bash
# check_upstream_release.sh - compare pinned upstream rsync version with the
# latest release announced at https://download.samba.org/pub/rsync/NEWS.
#
# Reads the pinned version from Cargo.toml workspace.metadata.oc_rsync.
# Writes "latest=<X.Y.Z>" and "pinned=<X.Y.Z>" to stdout.
#
# Exit codes:
#   0 - pinned version matches latest upstream release
#   1 - upstream is ahead of the pinned version
#   2 - error (network failure, parse failure, malformed inputs)
#
# Override the data sources via env for tests:
#   NEWS_URL       - URL or local path to the NEWS file (default: samba.org)
#   CARGO_TOML     - path to Cargo.toml (default: workspace root)

set -uo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
news_url="${NEWS_URL:-https://download.samba.org/pub/rsync/NEWS}"
cargo_toml="${CARGO_TOML:-${workspace_root}/Cargo.toml}"

fetch_news() {
    if [[ "${news_url}" == http://* || "${news_url}" == https://* ]]; then
        curl --fail --silent --show-error --location --max-time 30 "${news_url}"
    elif [[ -f "${news_url}" ]]; then
        cat "${news_url}"
    else
        return 1
    fi
}

news_body=$(fetch_news) || {
    echo "error: failed to fetch ${news_url}" >&2
    exit 2
}

latest=$(printf '%s\n' "${news_body}" \
    | grep -E -m1 '^# NEWS for rsync [0-9]+\.[0-9]+\.[0-9]+' \
    | awk '{print $5}')

if [[ -z "${latest}" ]]; then
    echo "error: could not parse latest version from NEWS" >&2
    exit 2
fi

if [[ ! -f "${cargo_toml}" ]]; then
    echo "error: ${cargo_toml} not found" >&2
    exit 2
fi

pinned=$(grep -E -m1 '^upstream_version[[:space:]]*=' "${cargo_toml}" \
    | sed -E 's/.*"([^"]+)".*/\1/')

if [[ -z "${pinned}" ]]; then
    echo "error: could not read upstream_version from ${cargo_toml}" >&2
    exit 2
fi

printf 'latest=%s\npinned=%s\n' "${latest}" "${pinned}"

if [[ "${latest}" == "${pinned}" ]]; then
    exit 0
fi
exit 1
