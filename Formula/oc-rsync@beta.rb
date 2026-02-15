class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.5"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.5/oc-rsync-0.5.5-darwin-x86_64-beta.tar.gz"
      sha256 "4228b64159f2f91e3e41899c49342f8f7e46f67389db778510a13d3bf3f74bf2"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.5/oc-rsync-0.5.5-darwin-aarch64-beta.tar.gz"
      sha256 "f7d5888c33878f22bbbca1fd5f0a5dd597069af667731c2370f17ccb8cb3defc"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
