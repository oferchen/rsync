class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.1"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.1/oc-rsync-0.5.1-darwin-x86_64.tar.gz"
      sha256 "5b77c8011fa0625d25736fac33cf02c094c0e453cb431874c7c9d27b9395074b"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.1/oc-rsync-0.5.1-darwin-aarch64.tar.gz"
      sha256 "49d32a3c20f513e30e2192c56ad6b78443c45fa3d37782c5f902bf8417c63b35"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
