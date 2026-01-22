class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.3"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.3/oc-rsync-0.5.3-darwin-x86_64.tar.gz"
      sha256 "a63650d907025103287e39164eb265d37299d58926ef63a62a440fafb28f356b"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.3/oc-rsync-0.5.3-darwin-aarch64.tar.gz"
      sha256 "2ea0a2db40c6a03ec7085c896e92f7ae32b9a429e68764d4e10a256828abc28f"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
