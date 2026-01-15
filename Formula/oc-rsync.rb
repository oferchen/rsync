class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.2"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.2/oc-rsync-0.5.2-darwin-x86_64.tar.gz"
      sha256 "55c9f99015981772e79edd14d5fae5fd391d1118a22bbf7642835dde509cdc28"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.2/oc-rsync-0.5.2-darwin-aarch64.tar.gz"
      sha256 "dd34a0e38380e97fb90a3c01492c149556e3b727312891255fc92b43337af6df"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
