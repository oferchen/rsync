class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.7"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.7/oc-rsync-0.5.7-darwin-x86_64-beta.tar.gz"
      sha256 "cc732a4f5375fd395f3453d53f2672f0d02ef22591c9ea210133234099137481"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.7/oc-rsync-0.5.7-darwin-aarch64-beta.tar.gz"
      sha256 "f88cb0a58a9a4e883516f1268d46bb2aa7dccbbef0acbf22cd3063d3b29b6d55"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
