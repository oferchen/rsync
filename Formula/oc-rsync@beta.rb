class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.2-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.3"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-x86_64-beta.tar.gz"
      sha256 "3466b2029760d4aecf94ac9b80415c397e46c05444bad3c0adf70eac3c901b8b"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-aarch64-beta.tar.gz"
      sha256 "76ed2485976801032b1436ee12c913a0fa73e5d031f16284fe824d794e749d04"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
