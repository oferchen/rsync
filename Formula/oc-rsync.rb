class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  version "v0.0.0-local"
  url "https://github.com/oferchen/rsync/releases/download/v0.0.0-local/oc-rsync-macos-amd64.tar.gz"
  sha256 "CHANGE_ME"
  license "GPL-3.0-or-later"

  def install
    bin.install "oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end

