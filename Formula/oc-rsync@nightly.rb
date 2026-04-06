class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.0"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.0/oc-rsync-0.6.0-darwin-x86_64-nightly.tar.gz"
      sha256 "5a9405a38983f555356a7aa1f4a11b87c452eabe298d3416b1b2c7c5c3e3129e"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.0/oc-rsync-0.6.0-darwin-aarch64-nightly.tar.gz"
      sha256 "0b3a45226f05f2b0c905e5dbf4d118480cf479b2b8bd0c5ecba48c4014e4b855"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
