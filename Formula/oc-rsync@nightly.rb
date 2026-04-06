class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.0"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.0/oc-rsync-0.6.0-darwin-x86_64-nightly.tar.gz"
      sha256 "c389980c5a316bea59f12a494da2f41aa7ddd1b672262e740b0ddab234b0b3d6"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.0/oc-rsync-0.6.0-darwin-aarch64-nightly.tar.gz"
      sha256 "b6aaf956e9cc515e41f7182b8a28d44c4a28f75c00940dfdb28294b2bd5ea551"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
