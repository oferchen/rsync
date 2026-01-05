class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.1"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.1/oc-rsync-0.5.1-darwin-x86_64.tar.gz"
      sha256 "7907f4a84257c55df95bed26fdeb8ead882d4d6581ebe5d0a82e60b5d0cc8381"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.1/oc-rsync-0.5.1-darwin-aarch64.tar.gz"
      sha256 "bf2422256760516958b1cd43bc68a4bc29b5d3cc2a2f08b6b03d83e3d9c2156e"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
