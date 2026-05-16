class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.2-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.2"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.2/oc-rsync-0.6.2-darwin-x86_64-beta.tar.gz"
      sha256 "5e0f3bdc6632abf2a51f37691522d4b3fc4cea67ec5777c969279806a702a552"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.2/oc-rsync-0.6.2-darwin-aarch64-beta.tar.gz"
      sha256 "749be8360edad14c677679bf6407ac3ba5a72907e62dd9971c5f23f6d978b8ab"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
