class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.2"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.2/oc-rsync-0.5.1-darwin-x86_64.tar.gz"
      sha256 "cddbbf1f3ec4b08f1b23f91053febd943d396d59e2e9a3f50effd537fa65f6e2"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.2/oc-rsync-0.5.1-darwin-aarch64.tar.gz"
      sha256 "4c98c1242c1bd8d9c5527387b8aeb0659eb32bf72fc726d3ddff4fd919ff2bf9"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
