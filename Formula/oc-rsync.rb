class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "3.4.1-rust"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v3.4.1-rust/oc-rsync-3.4.1-rust-darwin-x86_64.tar.gz"
      sha256 "9bf88cd605304b2b340a35a2e8366db5fde3fb048cf191f1b53d4256f8d3358c"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v3.4.1-rust/oc-rsync-3.4.1-rust-darwin-aarch64.tar.gz"
      sha256 "27f6486516a6d2a113f79bbe55cbf9b5f8b552b5a94d91a5d91de8749bbfc30e"
    end
  end

  def install
    bin.install "oc-rsync"
    bin.install "oc-rsyncd"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
