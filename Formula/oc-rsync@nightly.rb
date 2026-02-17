class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.6"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.6/oc-rsync-0.5.6-darwin-x86_64-nightly.tar.gz"
      sha256 "44fc9c0f3c063196e36635041e79e378bf99df6c10d5c0a6dc586dd851a92da8"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.6/oc-rsync-0.5.6-darwin-aarch64-nightly.tar.gz"
      sha256 "857de63598fdbffa57e5ee1a0b596045199766dccd1cb82df06f655ba7212624"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
