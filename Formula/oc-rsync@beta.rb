class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.2-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.3"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-x86_64-beta.tar.gz"
      sha256 "d5513544c421ffa1986c241408e7e111cd0519617ae69a1e0de3c50f46a41890"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-aarch64-beta.tar.gz"
      sha256 "cfe61f1be71bd51da68efe66504a83177035930e0e51c599248e0f4180eab719"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
