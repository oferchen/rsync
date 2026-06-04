class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.2-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.3"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-x86_64-nightly.tar.gz"
      sha256 "f08fd83548a3d30957bd9b056f25f4965a71b84124a4a87f27165fe7d67a324e"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-aarch64-nightly.tar.gz"
      sha256 "5918a5000a0fbb09b8946c78a013bbbbb411f5bb49dc86e578d1ecc2c3a85f15"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
