class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.3"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.3/oc-rsync-0.5.3-darwin-x86_64-nightly.tar.gz"
      sha256 "f19de646488e65834a51a1138221e13cef1d6f1a9270d265dfd2e743c3c2d458"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.3/oc-rsync-0.5.3-darwin-aarch64-nightly.tar.gz"
      sha256 "6820430a0044e59b15f0d4ec3b870f27033edcdb725d3e77fdb54438e29afb07"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
