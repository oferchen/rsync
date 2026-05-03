class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.1"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.1/oc-rsync-0.6.1-darwin-x86_64-beta.tar.gz"
      sha256 "58d0cc7d877a5fa9e9b980098627b160f2ef34da85afb3b77594a3fd183f800f"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.1/oc-rsync-0.6.1-darwin-aarch64-beta.tar.gz"
      sha256 "f31f716a3c2f02648d36fa3805f218c02bf8f44d6243f635a9553c54e589549f"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
