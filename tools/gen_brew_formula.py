#!/usr/bin/env python3
"""
tools/gen_brew_formula.py

Generate Formula/oc-rsync.rb from env vars that the CI extracted from the
actual release assets of https://github.com/oferchen/rsync.

We only emit platform blocks (macOS/Linux, arm/intel) that actually have both
a URL and a SHA. That way, if a release only ships darwin and not linux, the
formula is still valid.
"""

from pathlib import Path
import os

version = os.environ.get("VERSION")
if not version:
    raise SystemExit("VERSION env is required")

def have(name: str) -> bool:
    return bool(os.environ.get(name))

macos_arm_url = os.environ.get("MACOS_ARM_URL")
macos_arm_sha = os.environ.get("MACOS_ARM_SHA")

macos_intel_url = os.environ.get("MACOS_INTEL_URL")
macos_intel_sha = os.environ.get("MACOS_INTEL_SHA")

linux_arm_url = os.environ.get("LINUX_ARM_URL")
linux_arm_sha = os.environ.get("LINUX_ARM_SHA")

linux_intel_url = os.environ.get("LINUX_INTEL_URL")
linux_intel_sha = os.environ.get("LINUX_INTEL_SHA")

lines = []
lines.append("# frozen_string_literal: true")
lines.append("")
lines.append("class OcRsync < Formula")
lines.append('  desc "Rust-based rsync 3.4.1-compatible client/daemon from github.com/oferchen/rsync"')
lines.append('  homepage "https://github.com/oferchen/rsync"')
lines.append(f'  version "{version}"')
lines.append('  license "GPL-3.0-or-later"')
lines.append("")

# macOS
if (macos_arm_url and macos_arm_sha) or (macos_intel_url and macos_intel_sha):
    lines.append("  on_macos do")
    if macos_arm_url and macos_arm_sha:
        lines.append("    on_arm do")
        lines.append(f'      url "{macos_arm_url}"')
        lines.append(f'      sha256 "{macos_arm_sha}"')
        lines.append("    end")
    if macos_intel_url and macos_intel_sha:
        lines.append("    on_intel do")
        lines.append(f'      url "{macos_intel_url}"')
        lines.append(f'      sha256 "{macos_intel_sha}"')
        lines.append("    end")
    lines.append("  end")
    lines.append("")

# Linux
if (linux_arm_url and linux_arm_sha) or (linux_intel_url and linux_intel_sha):
    lines.append("  on_linux do")
    if linux_arm_url and linux_arm_sha:
        lines.append("    on_arm do")
        lines.append(f'      url "{linux_arm_url}"')
        lines.append(f'      sha256 "{linux_arm_sha}"')
        lines.append("    end")
    if linux_intel_url and linux_intel_sha:
        lines.append("    on_intel do")
        lines.append(f'      url "{linux_intel_url}"')
        lines.append(f'      sha256 "{linux_intel_sha}"')
        lines.append("    end")
    lines.append("  end")
    lines.append("")

lines.append("  def install")
lines.append('    dir = Dir["*"].find { |f| File.directory?(f) && f.downcase.include?("oc-rsync") }')
lines.append("    if dir")
lines.append("      Dir.chdir(dir) do")
lines.append('        bin.install "oc-rsync" if File.exist?("oc-rsync")')
lines.append('        bin.install "oc-rsyncd" if File.exist?("oc-rsyncd")')
lines.append("      end")
lines.append("    else")
lines.append('      bin.install "oc-rsync" if File.exist?("oc-rsync")')
lines.append('      bin.install "oc-rsyncd" if File.exist?("oc-rsyncd")')
lines.append("    end")
lines.append("  end")
lines.append("")
lines.append("  test do")
lines.append('    assert_match "3.4.1", shell_output("#{bin}/oc-rsync --version")')
lines.append("  end")
lines.append("end")
lines.append("")

outdir = Path("Formula")
outdir.mkdir(parents=True, exist_ok=True)
(outdir / "oc-rsync.rb").write_text("\n".join(lines), encoding="utf-8")
print("wrote Formula/oc-rsync.rb")
