class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.7"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.7/oc-rsync-0.5.7-darwin-x86_64-nightly.tar.gz"
      sha256 "a21ff3aaf0a62dd59283bf4678b9f65e757fc90f534370edb30ab54f52a982a1"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.7/oc-rsync-0.5.7-darwin-aarch64-nightly.tar.gz"
      sha256 "c292e4174b25eac47f837f8f8c0a213c4cc4e06375fc460ed397b2145c6fe924"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
