class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.1"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.1/oc-rsync-0.6.1-darwin-x86_64-nightly.tar.gz"
      sha256 "09a59f84e54cbec3cf3573d4bda1f2697d51135d498b62467248484689f639f2"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.1/oc-rsync-0.6.1-darwin-aarch64-nightly.tar.gz"
      sha256 "45534b71b8fa73d37662284c2a4790c5f4d24a5c0f8ee0960000dce537f08667"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
