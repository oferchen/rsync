class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.9"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.9/oc-rsync-0.5.9-darwin-x86_64-nightly.tar.gz"
      sha256 "6546293def00e215482c319796db553af9cd43424729514fa6577f33e992408d"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.9/oc-rsync-0.5.9-darwin-aarch64-nightly.tar.gz"
      sha256 "8e17c41a8436a5a29cb5d000ca1cd2f4281f64ee2150588176a8ee0c92254d72"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
