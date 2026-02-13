class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.4"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.4/oc-rsync-0.5.4-darwin-x86_64-nightly.tar.gz"
      sha256 "a5ee4b0259e47d3fc9b962a270809d84dad2c30103c436ede32f26efe89bf54d"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.4/oc-rsync-0.5.4-darwin-aarch64-nightly.tar.gz"
      sha256 "a42dc1e54adf43b1c0684b7bc7582730a67c6740ca6eada94ea1abd4b1a7345e"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
