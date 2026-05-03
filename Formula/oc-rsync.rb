class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.1"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.1/oc-rsync-0.6.1-darwin-x86_64.tar.gz"
      sha256 "c1057ff0e3135647823301b1a32b7b921d4f932356af35ca3292599bc18e7001"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.1/oc-rsync-0.6.1-darwin-aarch64.tar.gz"
      sha256 "70befea9c6bd4c75af2e6baf1cecffa0acd9418a27337c8f4d995fa3d82f34c6"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
