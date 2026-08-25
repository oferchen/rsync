class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.2-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.3"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-x86_64.tar.gz"
      sha256 "3bb17fe73c496ef26358bf999e0182a506bd66ed6b5b1e3c4af50af72c545b2a"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-aarch64.tar.gz"
      sha256 "aeee6bf40734d4f8747bd190a7bb217188812fcedff86b0d72ff23b6392de5a1"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
