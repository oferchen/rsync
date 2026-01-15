class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.2"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.2/oc-rsync-0.5.2-darwin-x86_64.tar.gz"
      sha256 "b39b7fa1308c7c5c935485f202a053d3a14b6715c8b10d82e023c622a732e6c4"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.2/oc-rsync-0.5.2-darwin-aarch64.tar.gz"
      sha256 "3b5bb289ff251a93038d4dba61d89f4382859d0c94a13f337cc4de37f77447af"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
