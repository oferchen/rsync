class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.3"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.3/oc-rsync-0.5.3-darwin-x86_64.tar.gz"
      sha256 "8bd9351bc1fdec86efc0ace90277897431371549c8aad8ba6801a67767b81e15"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.3/oc-rsync-0.5.3-darwin-aarch64.tar.gz"
      sha256 "a16c09b40a328fbe194dcf50779f2dd3ba713959f13fa61865ef29ca98a4a5af"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
