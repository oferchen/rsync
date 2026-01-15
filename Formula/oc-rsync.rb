class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.2"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.2/oc-rsync-0.5.2-darwin-x86_64.tar.gz"
      sha256 "368486fcee03c81c9ee05bf80a1c06913342dde34e069c2e4d6255d76a7eff92"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.2/oc-rsync-0.5.2-darwin-aarch64.tar.gz"
      sha256 "a5b09e7384afef87de180cb3873374188d96728219499c89cde096b8ff764ea8"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
