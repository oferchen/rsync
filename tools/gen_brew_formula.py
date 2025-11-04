#!/usr/bin/env python3
"""
tools/gen_brew_formula.py

Generate Formula/oc-rsync.rb from env vars populated by CI from the *actual*
GitHub release assets at https://github.com/oferchen/rsync.

We make this tolerant: if a platform is missing (e.g. no Linux ARM artifact),
we just don't emit that block instead of failing.
"""

import os
from pathlib import Path

version = os.environ.get("VERSION")
if not version:
    raise SystemExit("VERSION env is required")

# collect present platforms
platforms = {
    "macos_arm": {
        "url": os.environ.get("MACOS_ARM_URL"),
        "sha": os.environ.get("MACOS_ARM_SHA"),
    },
    "macos_intel": {
        "url": os.environ.get("MACOS_INTEL_URL"),
        "sha": os.environ.get("MACOS_INTEL_SHA"),
    },
    "linux_arm": {
        "url": os.environ.get("LINUX_ARM_URL"),
        "sha": os.environ.get("LINUX_ARM_SHA"),
    },
    "linux_intel": {
        "url": os.environ.get("LINUX_INTEL_URL"),
        "sha": os.environ.get("LINUX_INTEL_SHA"),
    },
}

lines = []
lines.append("# frozen_string_literal: true\n")
lines.append("class OcRsync < Formula")
lines.append('  desc "Rust-based rsync 3.4.1-compatible client/daemon from github.com/oferchen/rsync"')
lines.append('  homepage "https://github.com/oferchen/rsync"')
lines.append(f'  version "{version}"')
lines.append('  license "GPL-3.0-or-later"')
lines.append("")

# macOS block
macos_arm = platforms["macos_arm"]
macos_intel = platforms["macos_intel"]
if macos_arm["url"] or macos_intel["url"]:
    lines.append("  on_macos do")
    if macos_arm["url"] and macos_arm["sha"]:
        lines.append("    on_arm do")
        lines.append(f'      url "{macos_arm["url"]}"')
        lines.append(f'      sha256 "{macos_arm["sha"]}"')
        lines.append("    end")
    if macos_intel["url"] and macos_intel["sha"]:
        lines.append("    on_intel do")
        lines.append(f'      url "{macos_intel["url"]}"')
        lines.append(f'      sha256 "{macos_intel["sha"]}"')
        lines.append("    end")
    lines.append("  end")
    lines.append("")

# linux block
linux_arm = platforms["linux_arm"]
linux_intel = platforms["linux_intel"]
if linux_arm["url"] or linux_intel["url"]:
    lines.append("  on_linux do")
    if linux_arm["url"] and linux_arm["sha"]:
        lines.append("    on_arm do")
        lines.append(f'      url "{linux_arm["url"]}"')
        lines.append(f'      sha256 "{linux_arm["sha"]}"')
        lines.append("    end")
    if linux_intel["url"] and linux_intel["sha"]:
        lines.append("    on_intel do")
        lines.append(f'      url "{linux_intel["url"]}"')
        lines.append(f'      sha256 "{linux_intel["sha"]}"')
        lines.append("    end")
    lines.append("  end")
    lines.append("")

# install + test
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
