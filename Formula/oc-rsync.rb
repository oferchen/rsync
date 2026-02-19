class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.8"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.8/oc-rsync-0.5.8-darwin-x86_64.tar.gz"
      sha256 "b31cb20acddeeaeb02b3857f961399761688dd60fb014cff0004056901c80c48"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.8/oc-rsync-0.5.8-darwin-aarch64.tar.gz"
      sha256 "901e727ffd138c163946dadc5cf269e85b97826cdb21571538fbb2b5eed14ba7"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
