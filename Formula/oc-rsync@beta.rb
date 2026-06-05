class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.2-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.3"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-x86_64-beta.tar.gz"
      sha256 "76034111da96322db51b89867c3b5074f81a0bd29af31dce7da999256a44c5a5"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-aarch64-beta.tar.gz"
      sha256 "2b561ebb05167bb4501a37a3ab0a59f8333dc819805eeb4f3bac984c6e4c9f53"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
