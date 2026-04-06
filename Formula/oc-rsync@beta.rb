class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.0"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.0/oc-rsync-0.6.0-darwin-x86_64-beta.tar.gz"
      sha256 "4058603b978325a8bcbb9b2e73c2e4ef36162e2fc2b9fdbc83711991e31f5300"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.0/oc-rsync-0.6.0-darwin-aarch64-beta.tar.gz"
      sha256 "119b1e5c4a8daa1bec73d8fd52a8d66eff5dd130b6e279c3b22772856d64154e"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
