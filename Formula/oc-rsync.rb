class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.1"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.1/oc-rsync-0.5.1-darwin-x86_64.tar.gz"
      sha256 "5870d5c3352a7dad461304576899eb676638d4b91527f3118437e0148e56c58f"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.1/oc-rsync-0.5.1-darwin-aarch64.tar.gz"
      sha256 "28e4f887e815a2cd45e8826af517c4d1bb2d8de321f9f378f15f409cb31143e6"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
