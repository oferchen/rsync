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

if sys.version_info < (3, 11):
    print(
        f"[ERROR] Python 3.11+ is required for tomllib; found {sys.version.split()[0]}",
        file=sys.stderr,
    )
    sys.exit(1)

from pathlib import PurePosixPath

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
client_bin = ""
daemon_bin = ""
source_url = ""
daemon_config_dir = None
daemon_config = None
daemon_secrets = None

def expect_string(table: dict[str, object] | None, key: str) -> str:
    if table is None:
        return ""
    value = table.get(key) if isinstance(table, dict) else None
    if value is None:
        errors.append(f"workspace.metadata.oc_rsync.{key} is not set")
        return ""
    if not isinstance(value, str):
        errors.append(f"workspace.metadata.oc_rsync.{key} must be a string")
        return ""
    stripped = value.strip()
    if not stripped:
        errors.append(f"workspace.metadata.oc_rsync.{key} must not be empty")
    return stripped

if oc_metadata is None:
    errors.append("Cargo.toml missing [workspace.metadata.oc_rsync] table")
else:
    upstream_version = expect_string(oc_metadata, "upstream_version")
    client_bin = expect_string(oc_metadata, "client_bin")
    daemon_bin = expect_string(oc_metadata, "daemon_bin")
    daemon_wrapper_bin = ""
    if "daemon_wrapper_bin" in oc_metadata:
        daemon_wrapper_bin = expect_string(oc_metadata, "daemon_wrapper_bin")
    source_url = expect_string(oc_metadata, "source")

    config_dir_value = expect_string(oc_metadata, "daemon_config_dir")
    if config_dir_value:
        daemon_config_dir = PurePosixPath(config_dir_value)
        if daemon_config_dir.name == "":
            errors.append("workspace.metadata.oc_rsync.daemon_config_dir must not be the filesystem root")

    config_value = expect_string(oc_metadata, "daemon_config")
    if config_value:
        daemon_config = PurePosixPath(config_value)

    secrets_value = expect_string(oc_metadata, "daemon_secrets")
    if secrets_value:
        daemon_secrets = PurePosixPath(secrets_value)

if client_bin and daemon_bin and client_bin != daemon_bin:
    errors.append(
        "workspace.metadata.oc_rsync must configure a single binary so client_bin matches daemon_bin"
    )

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
    if source_url and source_url.rstrip("/") not in url:
        errors.append(
            "Formula url must originate from workspace.metadata.oc_rsync.source"
        )
    if crate_version and crate_version not in url:
        errors.append(
            "Formula url must include the crate version from Cargo.toml"
        )
    if upstream_version and upstream_version not in url:
        errors.append(
            "Formula url must include workspace.metadata.oc_rsync.upstream_version"
        )

if source_url:
    expected_homepage = f'homepage "{source_url}"'
    if expected_homepage not in text:
        errors.append(
            "Formula homepage must match workspace.metadata.oc_rsync.source"
        )

match_sha = re.search(r'sha256 "([0-9a-fA-F]{64})"', text)
if not match_sha:
    errors.append("Formula must provide a 64-character sha256 checksum")

forbidden_bins = ['bin/"rsync"', 'bin/"rsyncd"']
if any(needle in text for needle in forbidden_bins):
    errors.append("Formula must not install upstream rsync binary names")

required_bins = []
if client_bin:
    required_bins.append(f'bin.install "target/release/{client_bin}"')
missing_bins = [needle for needle in required_bins if needle not in text]
if missing_bins:
    errors.append("Formula must install oc-prefixed binaries via bin.install: " + ", ".join(missing_bins))



required_config_entries = []
if daemon_config_dir and daemon_config and daemon_secrets:
    config_dir_name = daemon_config_dir.name
    config_filename = daemon_config.name
    secrets_filename = daemon_secrets.name
    required_config_entries = [
        (
            f'(etc/"{config_dir_name}").install '
            f'"packaging/etc/{config_dir_name}/{config_filename}"'
        ),
        (
            f'(etc/"{config_dir_name}").install '
            f'"packaging/etc/{config_dir_name}/{secrets_filename}"'
        ),
    ]
missing_configs = [needle for needle in required_config_entries if needle not in text]
if missing_configs:
    errors.append(
        "Formula must install oc-rsyncd configuration assets: " + ", ".join(missing_configs)
    )

if daemon_config_dir and daemon_secrets:
    chmod_snippet = (
        f'chmod 0600, '
        f'etc/"{daemon_config_dir.name}/{daemon_secrets.name}"'
    )
    if chmod_snippet not in text:
        errors.append(
            "Formula must enforce 0600 permissions for oc-rsyncd.secrets via chmod"
        )

if daemon_config:
    examples_snippet = (
        f'(pkgshare/"examples").install '
        f'"packaging/examples/{daemon_config.name}"'
    )
    if examples_snippet not in text:
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
        (f'"--bin", "{client_bin}"', "build the oc-rsync binary"),
    ]:
        if flag not in build_block:
            errors.append(
                "Formula cargo build invocation must "
                f"{description}: missing {flag!r}"
            )

if client_bin and f'shell_output("#{{bin}}/{client_bin} --daemon --help")' not in text:
    errors.append(
        "Formula test block must exercise oc-rsync --daemon --help to validate single-binary daemon mode"
    )

if client_bin and f'shell_output("#{{bin}}/{client_bin} --version")' not in text:
    errors.append(
        "Formula test block must assert the --version banner for the branded binary"
    )

if errors:
    for error in errors:
        print(f"[ERROR] {error}")
    sys.exit(1)

print(f"Formula validation passed for {formula_path}")
PY
