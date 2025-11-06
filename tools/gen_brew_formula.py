#!/usr/bin/env python3
"""Generate the Homebrew formula for oc-rsync."""

from __future__ import annotations

import os
from pathlib import Path
from typing import Final

try:
    import tomllib
except ModuleNotFoundError as exc:  # pragma: no cover - Python <3.11 is unsupported in CI
    raise SystemExit("Python 3.11 or newer is required: {}".format(exc)) from exc


ROOT: Final = Path(__file__).resolve().parents[1]
FORMULA_DIR: Final = ROOT / "Formula"
FORMULA_PATH: Final = FORMULA_DIR / "oc-rsync.rb"


def read_crate_version() -> str:
    """Return the crate version declared in Cargo.toml."""

    cargo_toml = ROOT / "Cargo.toml"
    data = tomllib.loads(cargo_toml.read_text(encoding="utf-8"))
    package = data.get("package")
    if not package or "version" not in package:
        raise SystemExit("Cargo.toml must expose [package].version")
    version = str(package["version"]).strip()
    if not version:
        raise SystemExit("Cargo.toml [package].version cannot be empty")
    return version


def read_env_value(name: str) -> str:
    """Fetch a required environment variable with strict validation."""

    value = os.environ.get(name, "").strip()
    if not value:
        raise SystemExit(f"environment variable {name} is required")
    return value


def build_formula(version: str, tarball_url: str, sha256: str) -> str:
    """Render the oc-rsync Homebrew formula."""

    lines = [
        "# frozen_string_literal: true",
        "",
        "class OcRsync < Formula",
        '  desc "Pure-Rust rsync 3.4.1-compatible client/daemon installed as oc-rsync and oc-rsyncd"',
        '  homepage "https://github.com/oferchen/rsync"',
        f'  url "{tarball_url}"',
        f'  sha256 "{sha256}"',
        f'  version "{version}"',
        '  license "GPL-3.0-or-later"',
        "",
        '  depends_on "rust" => :build',
        "",
        "  def install",
        '    system "cargo", "build", "--release", "--locked", "--bin", "oc-rsync", "--bin", "oc-rsyncd"',
        '    bin.install "target/release/oc-rsync"',
        '    bin.install "target/release/oc-rsyncd"',
        "",
        '    (etc/"oc-rsyncd").install "packaging/etc/oc-rsyncd/oc-rsyncd.conf"',
        '    (etc/"oc-rsyncd").install "packaging/etc/oc-rsyncd/oc-rsyncd.secrets"',
        '    chmod 0600, etc/"oc-rsyncd/oc-rsyncd.secrets"',
        '    (pkgshare/"examples").install "packaging/examples/oc-rsyncd.conf"',
        "  end",
        "",
        "  test do",
        '    assert_match version.to_s, shell_output("#{bin}/oc-rsync --version")',
        "  end",
        "end",
        "",
    ]
    return "\n".join(lines)


def main() -> None:
    version = read_crate_version()
    override = os.environ.get("VERSION", "").strip()
    if override and override != version:
        raise SystemExit(
            "VERSION env ({}) must match Cargo.toml version ({})".format(override, version)
        )

    tarball_url = read_env_value("TARBALL_URL")
    sha256 = read_env_value("TARBALL_SHA256")

    formula = build_formula(version=version, tarball_url=tarball_url, sha256=sha256)

    FORMULA_DIR.mkdir(parents=True, exist_ok=True)
    FORMULA_PATH.write_text(formula, encoding="utf-8")
    print(f"wrote {FORMULA_PATH}")


if __name__ == "__main__":
    main()
