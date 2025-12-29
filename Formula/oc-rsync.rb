class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.0"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.0/oc-rsync-0.5.0-darwin-x86_64.tar.gz"
      sha256 "a7c97bfed1644418453064ab49dc2862806cf8513cd8c94230f5781e93a891c2"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.0/oc-rsync-0.5.0-darwin-aarch64.tar.gz"
      sha256 "674dd5c2e4e86f9c2cd7a22cb1a04d93bfd3487fb9dce855730688139e7c1985"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
