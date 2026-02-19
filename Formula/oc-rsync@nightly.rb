class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.8"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.8/oc-rsync-0.5.8-darwin-x86_64-nightly.tar.gz"
      sha256 "f006b686e287774eb30fd2649b4a05c17dd348b3866550d496a6031f6ac03f38"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.8/oc-rsync-0.5.8-darwin-aarch64-nightly.tar.gz"
      sha256 "9db654c1d2e0d480c8c6df7760e4764ae3016f46dedc408267a2b21bd71076b0"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
