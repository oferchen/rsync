class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.0"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.0/oc-rsync-0.5.0-darwin-x86_64.tar.gz"
      sha256 "930972a5536b0e0cf8603d5e66881c853328addd93c3139e5531ba7882d6f6af"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.0/oc-rsync-0.5.0-darwin-aarch64.tar.gz"
      sha256 "95143a9f284b9326bed7c6ecb6ce6dac73f25cb7a17c9bbecaa57b7a654c7b35"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
