class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.2-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.3"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-x86_64-nightly.tar.gz"
      sha256 "58d7583bfcb3fdf42fd738f6d70a09b16be1c3e009f2344dd90c2816332e2502"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-aarch64-nightly.tar.gz"
      sha256 "1693bc296c7781fa020ddd260196d729a6c467beae7a85cf5a6b3094f6b0779f"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
