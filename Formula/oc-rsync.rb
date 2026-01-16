class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.2"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.2/oc-rsync-0.5.2-darwin-x86_64.tar.gz"
      sha256 "c210ddf9ba80aeb04d77399636b279bc023d806d781d0a1c1ef5d1350cc5f43e"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.2/oc-rsync-0.5.2-darwin-aarch64.tar.gz"
      sha256 "70de3f739c9bea07970bd2fe898da18171d512123d46dbaab342f524791df54b"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
