class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "master"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/master/oc-rsync-3.4.1-rust-darwin-x86_64.tar.gz"
      sha256 "c60ce1de96cdc14b8ee16438fec516d893cc3e136f750ebb2d8cb499836a2918"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/master/oc-rsync-3.4.1-rust-darwin-aarch64.tar.gz"
      sha256 "153b420d271997bdae9b58f19ba81a167eea7aee9896ac104b54d7c0f0cf1a23"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
