class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.1"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.1/oc-rsync-0.5.0-darwin-x86_64.tar.gz"
      sha256 "17cb879238e824aeb07cecb0e80f6972a83cb8671c09342146b96dc27c8fbbe0"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.1/oc-rsync-0.5.0-darwin-aarch64.tar.gz"
      sha256 "28f2034dcc63273e0505bf48d991358f8ce46a166feb2f73666ff924231d1928"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
