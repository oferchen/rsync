class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.9"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.9/oc-rsync-0.5.9-darwin-x86_64-beta.tar.gz"
      sha256 "dede02ba74916a95a4107373015fa2907fd5a11e3a92749517860d9c2edec74a"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.9/oc-rsync-0.5.9-darwin-aarch64-beta.tar.gz"
      sha256 "236af385386e7e1d6c5aa7291cfe0dea072b046770f6280869d8c850a2f28de9"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
