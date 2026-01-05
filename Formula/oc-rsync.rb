class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.1"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.1/oc-rsync-0.5.0-darwin-x86_64.tar.gz"
      sha256 "4f6b5f199e9fa480259b7bd12d3a9f48548b30a54218495103e5ec1803c05075"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.1/oc-rsync-0.5.0-darwin-aarch64.tar.gz"
      sha256 "a5549ff6cd67ecc1dc6cbd320cc3cb2fb1d7305bafa4de33edae3d4f7cf01472"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
