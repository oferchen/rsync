class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.8"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.8/oc-rsync-0.5.8-darwin-x86_64-beta.tar.gz"
      sha256 "f2b8cbb491fbd38a628ca6c9b1a82970e18b7223653b93ae1cf0f2ad4e109457"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.8/oc-rsync-0.5.8-darwin-aarch64-beta.tar.gz"
      sha256 "bfb0bc32de76c3f17abc31c3191d29876e2ea43d95c95d29b26ea18f33ce54ae"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
