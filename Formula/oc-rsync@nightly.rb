class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.5"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.5/oc-rsync-0.5.5-darwin-x86_64-nightly.tar.gz"
      sha256 "49232208666ae49a696259061dab59ada625cd62cdb04f7cbb099cc8469f6685"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.5/oc-rsync-0.5.5-darwin-aarch64-nightly.tar.gz"
      sha256 "18efa14ceca64754b9dd897892f6f1d598fe651dc73339d87dc8a67de4acef64"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
