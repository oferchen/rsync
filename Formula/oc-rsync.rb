class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.3"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.3/oc-rsync-0.5.3-darwin-x86_64.tar.gz"
      sha256 "6f4177a96e45927168b5c120e25e6a78678ec4d44de78a101cece5dd7c731619"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.3/oc-rsync-0.5.3-darwin-aarch64.tar.gz"
      sha256 "b65945bc25f5898d123f5f5634ed54b552b1c2447b65b4948ad3a9e2a0ab5829"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
