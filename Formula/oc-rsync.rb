class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.7"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.7/oc-rsync-0.5.7-darwin-x86_64.tar.gz"
      sha256 "0ec3dd2a7206afb5180e70050ac3473ffbb3b538832ff93b86e02f038c75c00f"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.7/oc-rsync-0.5.7-darwin-aarch64.tar.gz"
      sha256 "c6b216520ecbcf3e9644366d9c321ea61b6164447316c518b067f347b2199781"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
