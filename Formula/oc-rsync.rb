class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.4-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.4"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.4/oc-rsync-0.6.4-darwin-x86_64.tar.gz"
      sha256 "604075f759bd43745823048f9894a9cda70b839a6935c34849fcbae0c8e1f2b8"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.4/oc-rsync-0.6.4-darwin-aarch64.tar.gz"
      sha256 "c59bde7f7a1c44c9c358f361d6b18398e9c759c7da43c0b4741021a3156d71c0"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
