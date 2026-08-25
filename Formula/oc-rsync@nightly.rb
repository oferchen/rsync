class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.2-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.3"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-x86_64-nightly.tar.gz"
      sha256 "9a3c2b53149f9f9ab412e7a2902876258e6755eec337e8c0b6f83f0151c1dd8d"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-aarch64-nightly.tar.gz"
      sha256 "6c4997597fe9e539022f8c03305f1e6a72009fae76787f19b9a791c13342d0dc"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
