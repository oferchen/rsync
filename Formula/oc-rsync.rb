class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.2-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.2"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.2/oc-rsync-0.6.2-darwin-x86_64.tar.gz"
      sha256 "f77f777a03e293e7b963a7c28c0071883d4bc41d898376ccb3e9449ced74ace4"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.2/oc-rsync-0.6.2-darwin-aarch64.tar.gz"
      sha256 "458a2b117aa3d51a9b700d585b3e81be686068c432dc6f5eb3752c630fa0c0d6"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
