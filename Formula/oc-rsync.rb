class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.8"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.8/oc-rsync-0.5.8-darwin-x86_64.tar.gz"
      sha256 "1fe47d96f2f1158bfde1cf9ed3ad373cb32533bb9803f48baae40e540fcaa2d2"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.8/oc-rsync-0.5.8-darwin-aarch64.tar.gz"
      sha256 "e2729ccd4c4eb57798dafa21f58ea0ec0aadf20cff2ad63a5f2059e6045a27d6"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
