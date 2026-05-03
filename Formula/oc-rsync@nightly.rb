class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.1"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.1/oc-rsync-0.6.1-darwin-x86_64-nightly.tar.gz"
      sha256 "b6d9bfa6ad93623ed15ac2569bb785e03e486ec0ec9d5e6a36452e8d4829c585"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.1/oc-rsync-0.6.1-darwin-aarch64-nightly.tar.gz"
      sha256 "01bab3ee78b13a6f61fb7bb5fa72c977f92689a0a3ffa5a3e0b9cf900fe42393"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
