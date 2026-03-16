class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.9"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.9/oc-rsync-0.5.9-darwin-x86_64-beta.tar.gz"
      sha256 "e7c122d6e9f4a1f0f7d039649e77625e1ecd0d3c4f87ca23eebb1a5495c4aad3"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.9/oc-rsync-0.5.9-darwin-aarch64-beta.tar.gz"
      sha256 "695ba73a69f32c44285df4200f60825956597ad72e087c0c61664fab638631a8"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
