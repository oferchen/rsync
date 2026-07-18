class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.4-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.4"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.4/oc-rsync-0.6.4-darwin-x86_64.tar.gz"
      sha256 "84e27fe6a2ca8da6af8c70949aa4cf6482c11d98c708c949ef744b6113f79022"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.4/oc-rsync-0.6.4-darwin-aarch64.tar.gz"
      sha256 "f17d1bfe5c81457d03257eb14cf06d095c0e7fcee6dfa9ed49a401bea6b61b28"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
