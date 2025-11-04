#!/usr/bin/env python3
"""
tools/gen_brew_formula.py

Generate Formula/oc-rsync.rb from actual release assets on GitHub.

CI must set these environment variables from the GitHub API output:

  VERSION
  MACOS_ARM_URL
  MACOS_ARM_SHA
  MACOS_INTEL_URL
  MACOS_INTEL_SHA
  LINUX_ARM_URL
  LINUX_ARM_SHA
  LINUX_INTEL_URL
  LINUX_INTEL_SHA

We don’t guess names — we use whatever CI found in the release.
"""

import os
from pathlib import Path

required = [
    "VERSION",
    "MACOS_ARM_URL",
    "MACOS_ARM_SHA",
    "MACOS_INTEL_URL",
    "MACOS_INTEL_SHA",
    "LINUX_ARM_URL",
    "LINUX_ARM_SHA",
    "LINUX_INTEL_URL",
    "LINUX_INTEL_SHA",
]
for key in required:
    if key not in os.environ:
        raise SystemExit("missing env: {}".format(key))

version = os.environ["VERSION"]

macos_arm_url = os.environ["MACOS_ARM_URL"]
macos_arm_sha = os.environ["MACOS_ARM_SHA"]
macos_intel_url = os.environ["MACOS_INTEL_URL"]
macos_intel_sha = os.environ["MACOS_INTEL_SHA"]

linux_arm_url = os.environ["LINUX_ARM_URL"]
linux_arm_sha = os.environ["LINUX_ARM_SHA"]
linux_intel_url = os.environ["LINUX_INTEL_URL"]
linux_intel_sha = os.environ["LINUX_INTEL_SHA"]

formula = f"""# frozen_string_literal: true

class OcRsync < Formula
  desc "Rust-based rsync 3.4.1-compatible client/daemon from github.com/oferchen/rsync"
  homepage "https://github.com/oferchen/rsync"
  version "{version}"
  license "GPL-3.0-or-later"

  on_macos do
    on_arm do
      url "{macos_arm_url}"
      sha256 "{macos_arm_sha}"
    end
    on_intel do
      url "{macos_intel_url}"
      sha256 "{macos_intel_sha}"
    end
  end

  on_linux do
    on_arm do
      url "{linux_arm_url}"
      sha256 "{linux_arm_sha}"
    end
    on_intel do
      url "{linux_intel_url}"
      sha256 "{linux_intel_sha}"
    end
  end

  def install
    # handle both "binary in root" and "one top-level dir" releases
    dir = Dir["*"].find {{ |f| File.directory?(f) && f.downcase.include?("oc-rsync") }}
    if dir
      Dir.chdir(dir) do
        bin.install "oc-rsync" if File.exist?("oc-rsync")
        bin.install "oc-rsyncd" if File.exist?("oc-rsyncd")
      end
    else
      bin.install "oc-rsync" if File.exist?("oc-rsync")
      bin.install "oc-rsyncd" if File.exist?("oc-rsyncd")
    end
  end

  test do
    assert_match "3.4.1", shell_output("\#{bin}/oc-rsync --version")
  end
end
"""

outdir = Path("Formula")
outdir.mkdir(parents=True, exist_ok=True)
(outdir / "oc-rsync.rb").write_text(formula, encoding="utf-8")
print("wrote Formula/oc-rsync.rb")

