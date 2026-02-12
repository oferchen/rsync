class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.4"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.4/oc-rsync-0.5.4-darwin-x86_64-beta.tar.gz"
      sha256 "79526312077e240ec939ba86b66d1f389c9f8e2945e2708e1e3d40909816c4fc"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.4/oc-rsync-0.5.4-darwin-aarch64-beta.tar.gz"
      sha256 "697019be8fe867ca2ac7f41356778d382ed1ff76b4d646f07fbe34b018eb4d1f"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
