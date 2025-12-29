class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.0"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.0/oc-rsync-0.5.0-darwin-x86_64.tar.gz"
      sha256 "81131f8cf5162eb018b63c8548467b20ad6ce26fb05ee0371259bbd5aed96d02"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.0/oc-rsync-0.5.0-darwin-aarch64.tar.gz"
      sha256 "7b1ea0a9d1628040531ce4d45e19b7cb28dd06d45594d09bcf7bc618353ffe81"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
