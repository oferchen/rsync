class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.4"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.4/oc-rsync-0.5.4-darwin-x86_64.tar.gz"
      sha256 "18c9c5345f73632963fd2f1f148ac8ca487f425c25d5451819146e70a5b33b6d"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.4/oc-rsync-0.5.4-darwin-aarch64.tar.gz"
      sha256 "c4162537c64d427624646641eae5518a6a837f167859893660238b48a494d17b"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
