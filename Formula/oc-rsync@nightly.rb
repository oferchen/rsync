class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.5"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.5/oc-rsync-0.5.5-darwin-x86_64-nightly.tar.gz"
      sha256 "7b6085c0f7a40e0021cd2d2aa4c0ca46b4cfb35e7217ed72d82ea20de241d948"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.5/oc-rsync-0.5.5-darwin-aarch64-nightly.tar.gz"
      sha256 "6b95b251a0e36ef6085ce0081785c20ed4de5cb9f418c7dc8c1a9cf55dfe2f71"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
