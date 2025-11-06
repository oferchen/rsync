#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "${SCRIPT_DIR}/.." && pwd)
FORMULA_PATH=${1:-"${REPO_ROOT}/Formula/oc-rsync.rb"}

if [[ ! -f "${FORMULA_PATH}" ]]; then
  echo "[ERROR] Formula not found: ${FORMULA_PATH}" >&2
  exit 1
fi

python3 - "${REPO_ROOT}" "${FORMULA_PATH}" <<'PY'
import pathlib
import re
import sys

import tomllib

root = pathlib.Path(sys.argv[1])
formula_path = pathlib.Path(sys.argv[2])
text = formula_path.read_text(encoding="utf-8")
errors: list[str] = []

if "class OcRsync < Formula" not in text:
    errors.append("Formula must declare class OcRsync < Formula")

cargo_data = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
crate_version = str(cargo_data["package"]["version"]).strip()
if not crate_version:
    errors.append("Cargo.toml version is empty")

match_version = re.search(r'version "([^"]+)"', text)
if not match_version:
    errors.append("Formula must declare version")
else:
    formula_version = match_version.group(1).strip()
    if formula_version != crate_version:
        errors.append(
            f"Formula version {formula_version!r} does not match Cargo.toml version {crate_version!r}"
        )

match_url = re.search(r'url "([^"]+)"', text)
if not match_url:
    errors.append("Formula must declare source url")
else:
    url = match_url.group(1)
    if "github.com/oferchen/rsync/archive/refs/tags/" not in url:
        errors.append("Formula url must reference the GitHub release tarball")
    sanitized = crate_version.replace("-rust", "")
    if sanitized and sanitized not in url:
        errors.append(f"Formula url must include sanitized crate version {sanitized!r}")

match_sha = re.search(r'sha256 "([0-9a-fA-F]{64})"', text)
if not match_sha:
    errors.append("Formula must provide a 64-character sha256 checksum")

forbidden_bins = ['bin/"rsync"', 'bin/"rsyncd"']
if any(needle in text for needle in forbidden_bins):
    errors.append("Formula must not install upstream rsync binary names")

required_bins = [
    'bin.install "target/release/oc-rsync"',
    'bin.install "target/release/oc-rsyncd"',
]
missing_bins = [needle for needle in required_bins if needle not in text]
if missing_bins:
    errors.append("Formula must install oc-prefixed binaries via bin.install: " + ", ".join(missing_bins))

for needle in ["oc-rsync", "oc-rsyncd"]:
    if needle not in text:
        errors.append(f"Formula must reference {needle} to satisfy packaging requirements")

if errors:
    for error in errors:
        print(f"[ERROR] {error}")
    sys.exit(1)

print(f"Formula validation passed for {formula_path}")
PY
