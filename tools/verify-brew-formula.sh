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

def get_table(data: object, *keys: str) -> dict[str, object] | None:
    current: object = data
    for key in keys:
        if not isinstance(current, dict):
            return None
        if key not in current:
            return None
        current = current[key]
    if not isinstance(current, dict):
        return None
    return current

oc_metadata = get_table(cargo_data, "workspace", "metadata", "oc_rsync")
upstream_version = ""
if oc_metadata is None:
    errors.append("Cargo.toml missing [workspace.metadata.oc_rsync] table")
else:
    value = oc_metadata.get("upstream_version")
    if value is None:
        errors.append("workspace.metadata.oc_rsync.upstream_version is not set")
    else:
        upstream_version = str(value).strip()
        if not upstream_version:
            errors.append("workspace.metadata.oc_rsync.upstream_version is empty")

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
    if crate_version and crate_version not in url:
        errors.append(
            "Formula url must include the crate version from Cargo.toml"
        )
    if upstream_version and upstream_version not in url:
        errors.append(
            "Formula url must include workspace.metadata.oc_rsync.upstream_version"
        )

match_sha = re.search(r'sha256 "([0-9a-fA-F]{64})"', text)
if not match_sha:
    errors.append("Formula must provide a 64-character sha256 checksum")

forbidden_bins = ['bin/"rsync"', 'bin/"rsyncd"']
if any(needle in text for needle in forbidden_bins):
    errors.append("Formula must not install upstream rsync binary names")

required_bins = [
    'bin.install "target/release/oc-rsync"',
]
missing_bins = [needle for needle in required_bins if needle not in text]
if missing_bins:
    errors.append("Formula must install oc-prefixed binaries via bin.install: " + ", ".join(missing_bins))

for needle in ["oc-rsync"]:
    if needle not in text:
        errors.append(f"Formula must reference {needle} to satisfy packaging requirements")

required_config_entries = [
    '(etc/"oc-rsyncd").install "packaging/etc/oc-rsyncd/oc-rsyncd.conf"',
    '(etc/"oc-rsyncd").install "packaging/etc/oc-rsyncd/oc-rsyncd.secrets"',
]
missing_configs = [needle for needle in required_config_entries if needle not in text]
if missing_configs:
    errors.append(
        "Formula must install oc-rsyncd configuration assets: " + ", ".join(missing_configs)
    )

if 'chmod 0600, etc/"oc-rsyncd/oc-rsyncd.secrets"' not in text:
    errors.append(
        "Formula must enforce 0600 permissions for oc-rsyncd.secrets via chmod"
    )

if '(pkgshare/"examples").install "packaging/examples/oc-rsyncd.conf"' not in text:
    errors.append(
        "Formula must install the sample oc-rsyncd.conf under pkgshare/examples"
    )

if 'depends_on "rust" => :build' not in text:
    errors.append(
        'Formula must declare depends_on "rust" => :build to ensure Cargo is available'
    )

build_match = re.search(r'system "cargo", "build"([^)]*)\)', text)
if build_match is None:
    errors.append('Formula must invoke cargo build in the install stanza')
else:
    build_block = build_match.group(0)
    for flag, description in [
        ("--release", "use --release for optimized binaries"),
        ("--locked", "pin Cargo.lock when building"),
        ('"--bin", "oc-rsync"', "build the oc-rsync binary"),
    ]:
        if flag not in build_block:
            errors.append(
                "Formula cargo build invocation must "
                f"{description}: missing {flag!r}"
            )

if 'shell_output("#{bin}/oc-rsync --daemon --help")' not in text:
    errors.append(
        "Formula test block must exercise oc-rsync --daemon --help to validate single-binary daemon mode"
    )

if errors:
    for error in errors:
        print(f"[ERROR] {error}")
    sys.exit(1)

print(f"Formula validation passed for {formula_path}")
PY
