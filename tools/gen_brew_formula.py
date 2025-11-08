#!/usr/bin/env python3
"""Generate the Homebrew formula for oc-rsync."""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Final

try:
    import tomllib
except ModuleNotFoundError as exc:  # pragma: no cover - Python <3.11 is unsupported in CI
    raise SystemExit("Python 3.11 or newer is required: {}".format(exc)) from exc


ROOT: Final = Path(__file__).resolve().parents[1]
FORMULA_DIR: Final = ROOT / "Formula"
FORMULA_PATH: Final = FORMULA_DIR / "oc-rsync.rb"


@dataclass(frozen=True)
class Branding:
    """Snapshot of workspace branding metadata."""

    client_bin: str
    daemon_bin: str
    daemon_wrapper_bin: str
    upstream_version: str
    rust_version: str
    source_url: str
    daemon_config_dir: PurePosixPath
    daemon_config: PurePosixPath
    daemon_secrets: PurePosixPath

    @property
    def config_dir_name(self) -> str:
        return self.daemon_config_dir.name

    @property
    def config_filename(self) -> str:
        return self.daemon_config.name

    @property
    def secrets_filename(self) -> str:
        return self.daemon_secrets.name

    @property
    def packaging_config_path(self) -> str:
        return f"packaging/etc/{self.config_dir_name}/{self.config_filename}"

    @property
    def packaging_secrets_path(self) -> str:
        return f"packaging/etc/{self.config_dir_name}/{self.secrets_filename}"

    @property
    def packaging_examples_path(self) -> str:
        return f"packaging/examples/{self.config_filename}"


def load_cargo_manifest() -> dict[str, object]:
    """Load the workspace Cargo manifest."""

    cargo_toml = ROOT / "Cargo.toml"
    return tomllib.loads(cargo_toml.read_text(encoding="utf-8"))


def read_crate_version(manifest: dict[str, object]) -> str:
    """Return the crate version declared in Cargo.toml."""

    package = manifest.get("package")
    if not package or "version" not in package:
        raise SystemExit("Cargo.toml must expose [package].version")
    version = str(package["version"]).strip()
    if not version:
        raise SystemExit("Cargo.toml [package].version cannot be empty")
    return version


def _metadata_table(manifest: dict[str, object]) -> dict[str, object]:
    workspace = manifest.get("workspace")
    if not isinstance(workspace, dict):
        raise SystemExit("Cargo.toml missing [workspace] table")
    metadata = workspace.get("metadata")
    if not isinstance(metadata, dict):
        raise SystemExit("Cargo.toml missing [workspace.metadata] table")
    oc_metadata = metadata.get("oc_rsync")
    if not isinstance(oc_metadata, dict):
        raise SystemExit("Cargo.toml missing [workspace.metadata.oc_rsync] table")
    return oc_metadata


def _expect_string(table: dict[str, object], key: str) -> str:
    value = table.get(key)
    if not isinstance(value, str):
        raise SystemExit(f"workspace.metadata.oc_rsync.{key} must be a string")
    value = value.strip()
    if not value:
        raise SystemExit(f"workspace.metadata.oc_rsync.{key} must not be empty")
    return value


def read_branding(manifest: dict[str, object]) -> Branding:
    """Load branding metadata from the workspace manifest."""

    metadata = _metadata_table(manifest)

    client_bin = _expect_string(metadata, "client_bin")
    daemon_bin = _expect_string(metadata, "daemon_bin")
    if client_bin != daemon_bin:
        raise SystemExit(
            "workspace.metadata.oc_rsync must configure a single binary: "
            f"client_bin ({client_bin}) must match daemon_bin ({daemon_bin})"
        )

    branding = Branding(
        client_bin=client_bin,
        daemon_bin=daemon_bin,
        daemon_wrapper_bin=_expect_string(metadata, "daemon_wrapper_bin"),
        upstream_version=_expect_string(metadata, "upstream_version"),
        rust_version=_expect_string(metadata, "rust_version"),
        source_url=_expect_string(metadata, "source"),
        daemon_config_dir=PurePosixPath(_expect_string(metadata, "daemon_config_dir")),
        daemon_config=PurePosixPath(_expect_string(metadata, "daemon_config")),
        daemon_secrets=PurePosixPath(_expect_string(metadata, "daemon_secrets")),
    )

    if branding.daemon_config_dir.name == "":
        raise SystemExit("daemon_config_dir must not resolve to the filesystem root")

    return branding


def read_env_value(name: str) -> str:
    """Fetch a required environment variable with strict validation."""

    value = os.environ.get(name, "").strip()
    if not value:
        raise SystemExit(f"environment variable {name} is required")
    return value


def build_formula(
    version: str,
    tarball_url: str,
    sha256: str,
    branding: Branding,
) -> str:
    """Render the oc-rsync Homebrew formula."""

    lines = [
        "# frozen_string_literal: true",
        "",
        "class OcRsync < Formula",
        (
            f'  desc "Pure-Rust rsync {branding.upstream_version}-compatible '
            f'client/daemon shipped as a single {branding.client_bin} binary"'
        ),
        f'  homepage "{branding.source_url}"',
        f'  url "{tarball_url}"',
        f'  sha256 "{sha256}"',
        f'  version "{version}"',
        '  license "GPL-3.0-or-later"',
        "",
        '  depends_on "rust" => :build',
        "",
        "  def install",
        (
            '    system "cargo", "build", "--release", "--locked", "--bin", '
            f'"{branding.client_bin}"'
        ),
        f'    bin.install "target/release/{branding.client_bin}"',
        "",
        (
            f'    (etc/"{branding.config_dir_name}").install '
            f'"{branding.packaging_config_path}"'
        ),
        (
            f'    (etc/"{branding.config_dir_name}").install '
            f'"{branding.packaging_secrets_path}"'
        ),
        (
            f'    chmod 0600, '
            f'etc/"{branding.config_dir_name}/{branding.secrets_filename}"'
        ),
        (
            f'    (pkgshare/"examples").install '
            f'"{branding.packaging_examples_path}"'
        ),
        "  end",
        "",
        "  test do",
        (
            f'    assert_match version.to_s, '
            f'shell_output("#{{bin}}/{branding.client_bin} --version")'
        ),
        (
            f'    assert_match "{branding.client_bin}", '
            f'shell_output("#{{bin}}/{branding.client_bin} --daemon --help")'
        ),
        "  end",
        "end",
        "",
    ]
    return "\n".join(lines)


def main() -> None:
    manifest = load_cargo_manifest()
    version = read_crate_version(manifest)
    branding = read_branding(manifest)
    override = os.environ.get("VERSION", "").strip()
    if override and override != version:
        raise SystemExit(
            "VERSION env ({}) must match Cargo.toml version ({})".format(override, version)
        )

    tarball_url = read_env_value("TARBALL_URL")
    sha256 = read_env_value("TARBALL_SHA256")

    formula = build_formula(
        version=version,
        tarball_url=tarball_url,
        sha256=sha256,
        branding=branding,
    )

    FORMULA_DIR.mkdir(parents=True, exist_ok=True)
    FORMULA_PATH.write_text(formula, encoding="utf-8")
    print(f"wrote {FORMULA_PATH}")


if __name__ == "__main__":
    main()
