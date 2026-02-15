class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.5"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.5/oc-rsync-0.5.5-darwin-x86_64.tar.gz"
      sha256 "b1619410847a63cb3596d23a5d1a2585ad39ae620a8c4f7c32cc2c9f354c000f"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.5/oc-rsync-0.5.5-darwin-aarch64.tar.gz"
      sha256 "4787b5a8727c3a68c50fbc7d66922f0a5c4033e38d6380032216de9d5b4874dc"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
