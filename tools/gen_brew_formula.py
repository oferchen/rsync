#!/usr/bin/env python3
"""
tools/gen_brew_formula.py

Generate a Homebrew formula (Formula/oc-rsync.rb) for the rsync project at
https://github.com/oferchen/rsync based on *actual* CI-provided environment
variables.

Environment variables expected (CI should set them):
- VERSION (required)
- MACOS_ARM_URL / MACOS_ARM_SHA
- MACOS_INTEL_URL / MACOS_INTEL_SHA
- LINUX_ARM_URL / LINUX_ARM_SHA
- LINUX_INTEL_URL / LINUX_INTEL_SHA

If a pair is missing (e.g. MACOS_ARM_URL without MACOS_ARM_SHA), that block
will not be emitted.

Output:
- Creates ./Formula/oc-rsync.rb relative to repository root.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Dict, Optional, List, Tuple


class EnvReader:
    """
    Small helper to read environment variables in a controlled way.
    This keeps knowledge about env var names in one place.
    """

    REQUIRED = ("VERSION",)

    PLATFORM_ENV_MAP: Dict[str, Tuple[str, str]] = {
        "macos_arm": ("MACOS_ARM_URL", "MACOS_ARM_SHA"),
        "macos_intel": ("MACOS_INTEL_URL", "MACOS_INTEL_SHA"),
        "linux_arm": ("LINUX_ARM_URL", "LINUX_ARM_SHA"),
        "linux_intel": ("LINUX_INTEL_URL", "LINUX_INTEL_SHA"),
    }

    def __init__(self) -> None:
        self._env = os.environ

    def ensure_required(self) -> None:
        for key in self.REQUIRED:
            if key not in self._env or not self._env[key]:
                raise SystemExit("missing required env: {}".format(key))

    def version(self) -> str:
        return self._env["VERSION"]

    def platform_value(self, key: str) -> Optional[Dict[str, str]]:
        """
        Return a mapping {"url": ..., "sha": ...} if both exist and are non-empty,
        otherwise return None to signal "do not emit this platform".
        """
        if key not in self.PLATFORM_ENV_MAP:
            return None
        url_env, sha_env = self.PLATFORM_ENV_MAP[key]
        url = self._env.get(url_env, "").strip()
        sha = self._env.get(sha_env, "").strip()
        if not url or not sha:
            return None
        return {"url": url, "sha": sha}


class FormulaBuilder:
    """
    Usage:
        fb = FormulaBuilder("3.4.1a-rust")
        fb.add_macos_block(arm=..., intel=...)
        fb.add_linux_block(arm=..., intel=...)
        formula_text = fb.build()
    """

    def __init__(self, version: str) -> None:
        self.version = version
        self._lines: List[str] = []
        self._header_written = False
        self._footer_written = False

    def _write_header(self) -> None:
        if self._header_written:
            return
        self._lines.append("# frozen_string_literal: true")
        self._lines.append("")
        self._lines.append("class OcRsync < Formula")
        self._lines.append('  desc "Rust-based rsync 3.4.1-compatible client/daemon from github.com/oferchen/rsync"')
        self._lines.append('  homepage "https://github.com/oferchen/rsync"')
        self._lines.append(f'  version "{self.version}"')
        self._lines.append('  license "GPL-3.0-or-later"')
        self._lines.append("")
        self._header_written = True

    def add_macos_block(
        self,
        arm: Optional[Dict[str, str]] = None,
        intel: Optional[Dict[str, str]] = None,
    ) -> None:
        self._write_header()
        if not arm and not intel:
            return
        self._lines.append("  on_macos do")
        if arm:
            self._lines.append("    on_arm do")
            self._lines.append(f'      url "{arm["url"]}"')
            self._lines.append(f'      sha256 "{arm["sha"]}"')
            self._lines.append("    end")
        if intel:
            self._lines.append("    on_intel do")
            self._lines.append(f'      url "{intel["url"]}"')
            self._lines.append(f'      sha256 "{intel["sha"]}"')
            self._lines.append("    end")
        self._lines.append("  end")
        self._lines.append("")

    def add_linux_block(
        self,
        arm: Optional[Dict[str, str]] = None,
        intel: Optional[Dict[str, str]] = None,
    ) -> None:
        self._write_header()
        if not arm and not intel:
            return
        self._lines.append("  on_linux do")
        if arm:
            self._lines.append("    on_arm do")
            self._lines.append(f'      url "{arm["url"]}"')
            self._lines.append(f'      sha256 "{arm["sha"]}"')
            self._lines.append("    end")
        if intel:
            self._lines.append("    on_intel do")
            self._lines.append(f'      url "{intel["url"]}"')
            self._lines.append(f'      sha256 "{intel["sha"]}"')
            self._lines.append("    end")
        self._lines.append("  end")
        self._lines.append("")

    def add_install_and_test(self) -> None:
        self._write_header()
        self._lines.append("  def install")
        self._lines.append('    dir = Dir["*"].find { |f| File.directory?(f) && f.downcase.include?("oc-rsync") }')
        self._lines.append("    if dir")
        self._lines.append("      Dir.chdir(dir) do")
        self._lines.append('        bin.install "oc-rsync" if File.exist?("oc-rsync")')
        self._lines.append('        bin.install "oc-rsyncd" if File.exist?("oc-rsyncd")')
        self._lines.append("      end")
        self._lines.append("    else")
        self._lines.append('      bin.install "oc-rsync" if File.exist?("oc-rsync")')
        self._lines.append('      bin.install "oc-rsyncd" if File.exist?("oc-rsyncd")')
        self._lines.append("    end")
        self._lines.append("  end")
        self._lines.append("")
        self._lines.append("  test do")
        self._lines.append('    assert_match "3.4.1", shell_output("#{bin}/oc-rsync --version")')
        self._lines.append("  end")

    def build(self) -> str:
        if not self._footer_written:
            if "def install" not in "\n".join(self._lines):
                self.add_install_and_test()
            self._lines.append("end")
            self._footer_written = True
        return "\n".join(self._lines) + "\n"


def main() -> None:
    env = EnvReader()
    env.ensure_required()

    version = env.version()

    macos_arm = env.platform_value("macos_arm")
    macos_intel = env.platform_value("macos_intel")
    linux_arm = env.platform_value("linux_arm")
    linux_intel = env.platform_value("linux_intel")

    builder = FormulaBuilder(version)
    builder.add_macos_block(arm=macos_arm, intel=macos_intel)
    builder.add_linux_block(arm=linux_arm, intel=linux_intel)
    builder.add_install_and_test()

    formula_text = builder.build()

    outdir = Path("Formula")
    outdir.mkdir(parents=True, exist_ok=True)
    outfile = outdir / "oc-rsync.rb"
    outfile.write_text(formula_text, encoding="utf-8")
    print(f"wrote {outfile}")


if __name__ == "__main__":
    main()
