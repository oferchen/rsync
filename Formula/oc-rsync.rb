class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.0"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.0/oc-rsync-0.5.0-darwin-x86_64.tar.gz"
      sha256 "dd1aeee9ccf1e9001dfe5629d7fd11b7fb4b98184797b041375f933db19d1820"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.0/oc-rsync-0.5.0-darwin-aarch64.tar.gz"
      sha256 "ca795cfc3fe455c6e9a09b713e782660c7eb4b926a5cad6f5fa89cd91ea9261c"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
