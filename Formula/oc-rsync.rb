class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "3.4.1-rust"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v3.4.1-rust/oc-rsync-3.4.1-rust-darwin-x86_64.tar.gz"
      sha256 "1accf8e57076106ff64025c0f0eb8dba6a395360dba8b6caf1d6ae223ceb1c38"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v3.4.1-rust/oc-rsync-3.4.1-rust-darwin-aarch64.tar.gz"
      sha256 "0dd6ccb12217bef7f424734e6323c5d8bc970cadc162daf4fc5f445c9ac94a33"
    end
  end

  def install
    bin.install "oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
