class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.0"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.0/oc-rsync-0.6.0-darwin-x86_64-beta.tar.gz"
      sha256 "6590e802fc544cff5d8f2b22e8e6ae9530bc1b3842a5b4d64c9487a71b2ac8db"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.0/oc-rsync-0.6.0-darwin-aarch64-beta.tar.gz"
      sha256 "843b019189eedcc4e64edc0e432cd0eb7a900c7532c9ae439ceba66ba275003f"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
