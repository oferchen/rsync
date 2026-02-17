class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.6"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.6/oc-rsync-0.5.6-darwin-x86_64-beta.tar.gz"
      sha256 "ade65c0244d3d69e993154305cde46e6669d53f87baa975ca9676621f5af0800"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.6/oc-rsync-0.5.6-darwin-aarch64-beta.tar.gz"
      sha256 "c71f07100a88a559c6293d42b4bc1595cd14a8acf1e411db41d6a046eb0013ba"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
