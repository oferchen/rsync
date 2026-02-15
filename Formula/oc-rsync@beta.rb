class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.5"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.5/oc-rsync-0.5.5-darwin-x86_64-beta.tar.gz"
      sha256 "c0c039851dcf795beee58e76331dcf1a8c781b038d8821e7c623d3911596e838"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.5/oc-rsync-0.5.5-darwin-aarch64-beta.tar.gz"
      sha256 "43d67305b995c9b5e07423ab14acd40cd533e579f14ea68450ae8924f1264119"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
