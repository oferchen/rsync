#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "${SCRIPT_DIR}/.." && pwd)
REPO="${OC_RSYNC_REPO:-oferchen/rsync}"
TOKEN="${GITHUB_TOKEN:-}"
TAG_OVERRIDE="${OC_RSYNC_FORMULA_TAG:-}"

usage() {
  cat <<'USAGE'
Usage: tools/update-brew-formula.sh [--repo <owner/repo>] [--tag <tag>] [--token <token>]

Determines the release tag to package, fetches the corresponding GitHub source
archive, computes its sha256 digest, and regenerates Formula/oc-rsync.rb using
tools/gen_brew_formula.py. The script writes useful outputs (tag, version,
sha256, tarball URL) to GITHUB_OUTPUT when available and verifies the generated
formula with tools/verify-brew-formula.sh.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      [[ $# -ge 2 ]] || { echo "--repo requires a value" >&2; exit 1; }
      REPO="$2"
      shift 2
      ;;
    --tag)
      [[ $# -ge 2 ]] || { echo "--tag requires a value" >&2; exit 1; }
      TAG_OVERRIDE="$2"
      shift 2
      ;;
    --token)
      [[ $# -ge 2 ]] || { echo "--token requires a value" >&2; exit 1; }
      TOKEN="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ -z "$REPO" ]]; then
  echo "[ERROR] Repository must be provided" >&2
  exit 1
fi

api_request() {
  local path="$1"
  local url="https://api.github.com${path}"
  local args=("-fsSL" "-H" "Accept: application/vnd.github+json")
  if [[ -n "$TOKEN" ]]; then
    args+=("-H" "Authorization: Bearer ${TOKEN}")
  fi
  curl "${args[@]}" "$url"
}

strip_refs_prefix() {
  local ref="$1"
  ref="${ref#refs/tags/}"
  ref="${ref#refs/heads/}"
  echo "$ref"
}

TAG="${TAG_OVERRIDE}"

if [[ -z "$TAG" && -n "${GITHUB_EVENT_PATH:-}" && -f "${GITHUB_EVENT_PATH:-}" ]]; then
  TAG=$(python3 - "$GITHUB_EVENT_PATH" <<'PY'
import json
import sys

def extract_tag(payload: dict) -> str:
    release = payload.get("release")
    if isinstance(release, dict):
        tag = release.get("tag_name")
        if isinstance(tag, str) and tag:
            return tag
    ref = payload.get("ref")
    if isinstance(ref, str) and ref:
        return ref
    inputs = payload.get("inputs")
    if isinstance(inputs, dict):
        tag = inputs.get("tag")
        if isinstance(tag, str) and tag:
            return tag
    return ""

path = sys.argv[1]
try:
    with open(path, "r", encoding="utf-8") as fh:
        data = json.load(fh)
except FileNotFoundError:
    print("", end="")
    raise SystemExit

print(extract_tag(data))
PY
  )
  TAG=$(strip_refs_prefix "${TAG}")
fi

if [[ -z "$TAG" ]]; then
  TAG=$(api_request "/repos/${REPO}/releases/latest" | python3 - <<'PY'
import json
import sys

data = json.load(sys.stdin)
print(data.get("tag_name", ""))
PY
  )
  TAG=$(strip_refs_prefix "${TAG}")
fi

if [[ -z "$TAG" ]]; then
  echo "[ERROR] Unable to determine release tag" >&2
  exit 1
fi

VERSION="${TAG#v}"
if [[ -z "$VERSION" ]]; then
  echo "[ERROR] Derived version is empty for tag '${TAG}'" >&2
  exit 1
fi

TARBALL_URL="https://github.com/${REPO}/archive/refs/tags/${TAG}.tar.gz"
TMP_DIR=$(mktemp -d)
trap 'rm -rf "${TMP_DIR}"' EXIT
TARBALL_PATH="${TMP_DIR}/source.tar.gz"

if ! curl -fsSL "${TARBALL_URL}" -o "${TARBALL_PATH}"; then
  echo "[ERROR] Failed to download ${TARBALL_URL}. Ensure the tag exists and is publicly accessible." >&2
  exit 1
fi
TARBALL_SHA256=$(sha256sum "${TARBALL_PATH}" | awk '{print $1}')

if [[ -z "${TARBALL_SHA256}" ]]; then
  echo "[ERROR] Failed to compute sha256 for ${TARBALL_PATH}" >&2
  exit 1
fi

export VERSION
export TARBALL_URL
export TARBALL_SHA256

python3 "${SCRIPT_DIR}/gen_brew_formula.py"
"${SCRIPT_DIR}/verify-brew-formula.sh" "${REPO_ROOT}/Formula/oc-rsync.rb"

echo "Updated Formula/oc-rsync.rb for tag ${TAG} (${VERSION})"

test -n "${GITHUB_OUTPUT:-}" && {
  {
    echo "tag=${TAG}"
    echo "version=${VERSION}"
    echo "tarball_url=${TARBALL_URL}"
    echo "tarball_sha256=${TARBALL_SHA256}"
  } >> "${GITHUB_OUTPUT}"
}
