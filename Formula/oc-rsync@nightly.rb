class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.9"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.9/oc-rsync-0.5.9-darwin-x86_64-nightly.tar.gz"
      sha256 "fa5a33fe75ee06e1b9372071f1911b82d994ec4499c9b978ee89ec4c99d5161e"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.9/oc-rsync-0.5.9-darwin-aarch64-nightly.tar.gz"
      sha256 "749523928c2a1a8a5586020fbf60fc21c553a1c4d70eb28acef64c684ec3e776"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
