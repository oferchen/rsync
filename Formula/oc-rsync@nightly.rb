class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.8"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.8/oc-rsync-0.5.8-darwin-x86_64-nightly.tar.gz"
      sha256 "d86aa1f2f11f0afae4bcb972ab7de8cab8873a90d59673add1b01f3bb983e1ae"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.8/oc-rsync-0.5.8-darwin-aarch64-nightly.tar.gz"
      sha256 "578b0e5e0c43deaca08d50244de565e2299b9ed0a4fafed782250e361b3d827f"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
