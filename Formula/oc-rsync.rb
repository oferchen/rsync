class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.0"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.0/oc-rsync-0.6.0-darwin-x86_64.tar.gz"
      sha256 "8506852bac597915ea33ca798ae6fd214c4166dd78634baa8538806d97f97eac"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.0/oc-rsync-0.6.0-darwin-aarch64.tar.gz"
      sha256 "3ef274b266742982d409c38b62827e350f52620b091f6ab21369a797701fb87c"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
