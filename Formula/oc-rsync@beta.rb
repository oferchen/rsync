class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.2-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.2"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.2/oc-rsync-0.6.2-darwin-x86_64-beta.tar.gz"
      sha256 "ad9ced9ed7e01f26b16ce736874bb25604155607b84fc8d2f504ecba96ef61f5"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.2/oc-rsync-0.6.2-darwin-aarch64-beta.tar.gz"
      sha256 "70de51f52416dec28ce21aa18efa71a962b3c71529deb7c4985d3a0ca0262b58"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
