class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.2"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.2/oc-rsync-0.5.0-darwin-x86_64.tar.gz"
      sha256 "d98a950a8dde3e2e330db0cf8ca980bee3d123b619777c0d77f3ed2f56d5bfe6"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.2/oc-rsync-0.5.0-darwin-aarch64.tar.gz"
      sha256 "9531b1119314d42455217ef65deb6b7af0e69fa950cb9a81e0b69f72fa692b7e"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
