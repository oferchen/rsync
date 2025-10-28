#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")"/.. && pwd)"
export OC_RSYNC_WORKSPACE_ROOT="$ROOT_DIR"
cd "$ROOT_DIR"

python3 <<'PY'
import json
import os
import pathlib
import subprocess
import sys

root = pathlib.Path(os.environ["OC_RSYNC_WORKSPACE_ROOT"])

try:
    metadata_output = subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=root,
    )
except subprocess.CalledProcessError as error:
    raise SystemExit(f"cargo metadata failed with exit code {error.returncode}") from error

metadata = json.loads(metadata_output)
oc_metadata = metadata.get("metadata", {}).get("oc_rsync")
if not isinstance(oc_metadata, dict):
    raise SystemExit("workspace.metadata.oc_rsync missing from Cargo.toml")

required_keys = {
    "brand",
    "upstream_version",
    "rust_version",
    "protocol",
    "client_bin",
    "daemon_bin",
    "legacy_client_bin",
    "legacy_daemon_bin",
    "daemon_config_dir",
    "daemon_config",
    "daemon_secrets",
    "legacy_daemon_config_dir",
    "legacy_daemon_config",
    "legacy_daemon_secrets",
    "source",
}
missing = sorted(required_keys.difference(oc_metadata))
if missing:
    raise SystemExit(
        "workspace.metadata.oc_rsync missing required keys: " + ", ".join(missing)
    )

brand = oc_metadata["brand"]
if brand != "oc":
    raise SystemExit(f"workspace brand must be 'oc', found {brand!r}")

upstream_version = oc_metadata["upstream_version"]
if upstream_version != "3.4.1":
    raise SystemExit(
        "upstream_version must remain aligned with rsync 3.4.1; "
        f"found {upstream_version!r}"
    )

rust_version = oc_metadata["rust_version"]
if not rust_version.endswith("-rust"):
    raise SystemExit(
        f"Rust-branded version should end with '-rust'; found {rust_version!r}"
    )

protocol = oc_metadata["protocol"]
if protocol != 32:
    raise SystemExit(f"Supported protocol must be 32; found {protocol}")

client_bin = oc_metadata["client_bin"]
daemon_bin = oc_metadata["daemon_bin"]
if not client_bin.startswith("oc-"):
    raise SystemExit(f"client_bin must start with 'oc-'; found {client_bin!r}")
if not daemon_bin.startswith("oc-"):
    raise SystemExit(f"daemon_bin must start with 'oc-'; found {daemon_bin!r}")

config_dir = pathlib.Path(oc_metadata["daemon_config_dir"])
config_path = pathlib.Path(oc_metadata["daemon_config"])
secrets_path = pathlib.Path(oc_metadata["daemon_secrets"])

for path, label in ((config_path, "daemon_config"), (secrets_path, "daemon_secrets")):
    if not path.is_absolute():
        raise SystemExit(f"{label} must be an absolute path; found {path}")
    if path.parent != config_dir:
        raise SystemExit(
            f"{label} {path} must reside within configured directory {config_dir}"
        )

if config_path.name == secrets_path.name:
    raise SystemExit("daemon configuration and secrets paths must not collide")

packaging_assets = root / "packaging" / "etc" / "oc-rsyncd"
expected_assets = {
    config_path.name: packaging_assets / config_path.name,
    secrets_path.name: packaging_assets / secrets_path.name,
}
missing_assets = [name for name, file in expected_assets.items() if not file.exists()]
if missing_assets:
    raise SystemExit(
        "packaging assets missing for: " + ", ".join(sorted(missing_assets))
    )

package_versions = {
    package["name"]: package["version"] for package in metadata.get("packages", [])
}
for crate_name in ("oc-rsync-bin", "oc-rsyncd-bin"):
    version = package_versions.get(crate_name)
    if version is None:
        raise SystemExit(f"crate {crate_name} missing from metadata")
    if version != rust_version:
        raise SystemExit(
            f"crate {crate_name} version {version} does not match {rust_version}"
        )

print("Preflight checks passed: branding, version, and packaging metadata validated.")
PY
