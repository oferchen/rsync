class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "3.4.1-rust"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v3.4.1-rust/oc-rsync-3.4.1-rust-darwin-x86_64.tar.gz"
      sha256 "d97197f4e8a71cc13a5476f687f9ab64ec84bfc8a28418136ecfa9c2c8fdeb49"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v3.4.1-rust/oc-rsync-3.4.1-rust-darwin-aarch64.tar.gz"
      sha256 "e9b02ab14decb3985f89598cf3d0ce758b2bf7fbd779fb3722320cc5eb3380cf"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
